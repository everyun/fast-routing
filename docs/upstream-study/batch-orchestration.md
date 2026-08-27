# Batch Routing Orchestration, Pass Scheduling, and v2 Multithreading

Upstream study of Freerouting's batch orchestration layer, written as an implementation-ready
spec for the Rust rewrite. Engineers reading this are assumed to NOT have the Java source.

Source trees studied (read completely):

* **v1.9 ("v19")** — `/tmp/fr-java/src_v19/main/java/app/freerouting/`
  * `autoroute/BatchAutorouter.java` (789 L), `BatchFanout.java` (312 L),
    `BatchOptRoute.java` (573 L), `BatchOptRouteMT.java` (376 L), `OptimizeRouteTask.java` (131 L),
    `ItemSelectionStrategy.java`, `BoardUpdateStrategy.java`, `ItemAutorouteInfo.java`,
    `ItemRouteResult.java`
  * `interactive/BatchAutorouterThread.java` (264 L), `interactive/InteractiveActionThread.java`
* **v2.x ("v2", the 2.3.0 line)** — `/tmp/fr-java/src/main/java/app/freerouting/`
  * `autoroute/pipeline/` — `RoutingPipeline`, `AutorouteBatchLoop`, `AutoroutePassRunner`,
    `BatchAutorouter`, `BatchAutorouterThread`, `AutorouteConnectionRouter`,
    `AutorouteAirlineCalculator`, `AutorouteRuntimeMetrics`, `AutorouteUnroutedReport`,
    `BatchFanout`, `BatchOptimizer`, `BatchOptimizerMultiThreaded`, `OptimizeRouteTask`,
    `NamedAlgorithm`, `NamedAlgorithmType`, `TaskState`
  * `autoroute/AutorouteAttemptResult.java`, `AutorouteAttemptState.java`, `BoardHistory.java`,
    `ItemRouteResult.java`, plus scoring (`core/scoring/BoardStatistics.java`) and settings
    (`settings/*.java`, `settings/sources/DefaultSettings.java`).

**Which code does the 2.3.0 release actually run?** In v2 the batch classes live under
`autoroute/pipeline/` (the top-level `autoroute/` classes named like v19 — `BatchAutorouter` etc. —
do not exist there anymore; only support types remain). The pipeline is entered from exactly two
places (verified by grep):

* Headless / API jobs: `management/jobs/RoutingJobSchedulerActionThread.java:99`
  → `RoutingPipeline.createForHeadless(job)`; `:167` → `pipeline.run()`.
* GUI jobs: `gui/workspace/progress/GuiRoutingJobWorker.java:212`
  → `RoutingPipeline.createForGui(routingJob)`; `:639` → `pipeline.run()`.

So v2's `pipeline/*` is authoritative for 2.3.0. Where v19 differs I call it out inline.

---

## 1. Top-level pipeline structure

### 1.1 v2: `RoutingPipeline` (pipeline/RoutingPipeline.java)

Stages run strictly sequentially on the job's worker thread (a `StoppableThread`):

```
run():
  runRoutingStage():                        # fanout + route are ONE stage
      if routerEnabled (settings.getRunRouter() && (maxPasses == null || maxPasses >= 0))
         and !stopAutoRouterRequested:
             autorouter.runBatchLoop()      # fanout pre-pass happens INSIDE the batch loop
      else if settings.isFanoutEnabled() and !stopRequested:
             maxPasses := 0 temporarily; autorouter.runBatchLoop(); restore maxPasses
             # trick: pass limit 0 => loop body runs fanout then immediately stops routing
      job.board.finishAutoroute()
      fire StageListener.afterRouting
  runOptimizationStage():
      skip if optimizer == null (getRunOptimizer() false) or thread.isStopRequested()
      job.stage = OPTIMIZATION; fire beforeOptimization
      optimizer.runBatchLoop()
      fire afterOptimization
  job.stage = IDLE
```

Optimizer construction policy (`BatchOptimizer.createForGui` / `createForHeadless`,
pipeline/BatchOptimizer.java:51-66):

* **Headless: ALWAYS single-threaded `BatchOptimizer`.**
* GUI: `BatchOptimizerMultiThreaded` iff `globalSettings.featureFlags.multiThreading`
  (default `true`, settings/FeatureFlagsSettings.java:11) AND
  `optimizer.maxThreads > 1` (default `max(1, cores-1)`, DefaultSettings.java:134).

Unsupported algorithm ids are normalized to the built-in with a warning
(RoutingPipeline.normalizeRouterAlgorithm, BatchOptimizer.normalizeAlgorithm).

### 1.2 v19: `interactive/BatchAutorouterThread.thread_action()`

```
fanout_first = autoroute_settings.get_with_fanout() && get_start_pass_no() <= 1
if fanout_first:            BatchFanout.fanout_board(thread)         # separate phase, BEFORE router
if with_autoroute && !stop: batch_autorouter.autoroute_passes(...)
routing_board.finish_autoroute()
if num_threads > 0 && with_autoroute && with_postroute && !stop:
    batch_opt_route.optimize_board(save_intermediate_stages,
                                   optimization_improvement_threshold, thread)
```

Optimizer choice (v19 BatchAutorouterThread.java:47-56): `BatchOptRouteMT` iff
`num_threads > 1`, else `BatchOptRoute`. **v19 ships with `num_threads = 1` by default**
(gui/StartupOptions.java:30; the commented-out line 28 shows it *used to be* cores-1) and prints
this hard warning when MT is requested (BatchAutorouterThread.java:49):

> "Multi-threaded route optimization is broken and it is known to generate clearance violations.
> It is highly recommended to use the single-threaded route optimization instead … '-mt 1' …"

This is the single most important upstream statement about parallelism: **the only parallel
component upstream ever shipped (the MT optimizer) is declared broken by its own authors in v19,
and is disabled by default.** See §8/§9.

---

## 2. Fanout stage (pin escape)

### 2.1 Orchestration

v19 `BatchFanout.fanout_board` / v2 `BatchFanout.fanoutBoard` (static entry points).

Setup (both versions):

1. Collect all SMD pins of the board (`board.getSmdPins()`; a pin is "SMD" when it exists on a
   single layer). v2 additionally filters out netless pins (pipeline/BatchFanout.java:46-51).
2. Group pins by component. Per component compute the **gravity center of its SMD pins**
   (arithmetic mean of pin centers).
3. Sort **components descending by SMD pin count**, tie-break ascending component id
   (`Component.compareTo`) — big BGAs/QFNs fan out first while the board is empty.
4. Sort pins within a component. v19: ascending `distance_to_component_center`
   ("inner_first" — pins nearest the centroid escape first; tie-break pin number).
   v2: configurable via `fanout.pinSortingOrder`:
   * `"inner_first"` (v1.9 behavior), `"outer_first"` (**v2 default**, DefaultSettings.java:125),
   * `"distanceToClosestOnNet"` — ascending distance to nearest same-net pin
     (precomputed O(P²) over all board pins, pipeline/BatchFanout.java:711-723),
   * `"surroundingsDensity"` — descending count of other SMD pins within 20.0 mm
     (pipeline/BatchFanout.java:726-738),
   * anything else = pin-index order. Tie-break always `pinIndex`.

Pass loop:

```
for pass i in 0 .. maxPasses-1:                # v19: fixed MAX_PASS_COUNT = 20
                                               # v2: settings.fanout.maxPasses (default 20)
    routedCount = fanoutPass(i)
    stop when routedCount == 0                 # v19 also requires insertErrorCount == 0
    v2-only extra stop conditions (pipeline/BatchFanout.java:110-157):
      - deadline (settings.fanout.timeout) reached -> isTimedOut
      - totalItemsFanouted >= fanout.maxItems (default MAX_INT)
      - oscillation detector: pack (routedCount<<32 ^ viaCount) into a long; if identical for
        3 consecutive passes ("stagnationPassLimit = 3"), stop. Comment records the motivating
        failure: 14 identical passes where pins kept ripping each other's escapes.
      - board hash unchanged since previous pass -> stop
```

Per pass (`fanoutPass(passNo)`):

* **Ripup cost schedule: `ripupCosts = startRipupCosts * (passNo + 1)`** with
  `startRipupCosts` default **100** (v2 DefaultSettings.DEFAULT_START_RIPUP_COSTS = 100;
  v19 reads the same setting from `autoroute_settings.get_start_ripup_costs()`).
  v2: `fanout.ripupAllowed=false` sends `-1`, which `RoutingBoard.fanout` treats as "no ripup".
* **Per-pin time limit: `maxMilliseconds = basePerPin * (passNo + 1)`**, base 10 000 ms
  (v19 hardcoded `10000 * (p_pass_no + 1)`, BatchFanout.java:181; v2
  `fanout.maxMillisecondsPerPin` default 10000L).
* For each component (in order), for each pin (in order):
  `board.startMarkingChangedArea(); result = board.fanout(pin, settings, ripupCosts, thread, timeLimit)`
  and count ROUTED / FAILED(NOT_ROUTED) / INSERT_ERROR / ALREADY_CONNECTED.
* v2 skips pins whose net class defines no vias and `fallbackToBoardVias` is off
  (pipeline/BatchFanout.java:238-258).
* Stop-checks between pins: user stop request; v2 also deadline and `fanout.maxItems`.

### 2.2 The actual escape algorithm (`RoutingBoard.fanout`) — how via positions are picked

v19 board/RoutingBoard.java:1080; v2 board/facade/RoutingBoard.java:978. Precondition: the pin
and its entire connected set live on exactly one layer, and the pin has exactly one net;
otherwise return ALREADY_CONNECTED. If `getUnconnectedSet(net)` is empty →
ALREADY_CONNECTED (v19) / NO_UNCONNECTED_NETS (v2).

There is **no explicit via-position search**. Fanout reuses the ordinary maze router with
`AutorouteControl.isFanout = true` and three special behaviors inside the maze engine:

1. **Destination = anywhere.** `MazeSearchEngine.init` (v2 maze/MazeSearchEngine.java:988;
   v19 MazeSearchAlgo.java:1275): if fanout, the destination-distance estimator is seeded with
   the *board bounding box* on the bottom and top layers, so the A*-style lower bound pulls the
   wave toward "any other layer" rather than a concrete target.
2. **Completion at the first via.** During expansion
   (v2 MazeSearchEngine.java:361-368; v19 MazeSearchAlgo.java:254-261): when the popped list
   element's door is an `ExpansionDrill` AND its backtrack door is also an `ExpansionDrill`,
   the search terminates with that drill as the destination — i.e. **the escape is complete the
   moment the route has passed down through a drill (via) to another layer**. The via position is
   therefore chosen implicitly by maze cost: the cheapest reachable drill location given trace
   costs, via cost, and ripup costs. (`ctrl.removeUnconnectedVias = false` so the placed via
   survives tail removal.)
3. **v2-only escape-length clamps** (maze/MazeSearchEngine.java:88-126, an overridden
   `TreeSet.add` on the expansion list): while on the start-pin's layer, expansion entries whose
   shape-entry midpoint is farther than `fanout.maxEscapeLengthMm` (default **4.5 mm**, fallback
   3.0 mm if unset) from the pin center are rejected; drill entries closer than
   `fanout.minEscapeLengthMm` (default **2.5 mm**, fallback 0.5 mm) are rejected. So v2
   constrains the via into an annulus around the pin. v19 has no such constraint.
   v2 also supports dedicated fanout via diameters (`fanout.startViaDiameterMm` /
   `endViaDiameterMm`, default 0.250 mm) and a `fallbackToBoardVias` combined via rule
   (facade/RoutingBoard.java:1025-1046).
4. **Fanout-via protection during later routing**: v19 MazeSearchAlgo.java:1443-1463 — when the
   main router considers ripping a via whose only contacts are traces, it multiplies the ripup
   cost by a "fanout via cost factor" > 1 so fanout vias are expensive to destroy
   (only when NOT in a fanout pass and `remove_unconnected_vias == false`).

v2 target-selection micro-optimization (facade/RoutingBoard.java:1003-1023, 1066-1095): sort the
unconnected set by squared distance of bounding-box center to the pin; if ≤ 4 targets, first try
routing to only the closest target, falling back to the whole set; if > 4 (power/ground nets),
route against the whole set at once "to avoid CPU thrashing".

After ROUTED: `optChangedArea` (pull-tight) with a **1000 ms** time limit
("TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP", same constant everywhere).

### 2.3 Escape statistics / exit report

A pin counts as *escaped* if it has a contacting Trace with no clearance violations, or a
contacting clean Via that itself contacts a Trace (v19 BatchFanout.isPinEscaped:74-96; v2 gets
the same from `BoardStatistics.fanout`). The phase log line format
(`"Fanout stage %s started with %d total SMD pins … escaped pins: %d/%d (%.1f%%) …"`) matches
between versions.

---

## 3. Route stage — item selection within a pass

`getAutorouteItems` (v2 pipeline/BatchAutorouter.java:345-409; v19 inline in
`autoroute_pass`, BatchAutorouter.java:335-373). Semantics:

* Iterate the board's `itemList` (an `UndoableObjects` insertion-ordered persistent list) in
  **storage order** — this IS the pass ordering; there is no geometric or net ordering of route
  work. Determinism comes from board insertion order.
* For each item that is `Connectable`, an `Item`, and `!isRoutable()` (i.e. a *component of
  connectivity* like a Pin or an existing trace end, not a free routable wire):
  * skip if already in `handledItems`;
  * for each net of the item: compute `connectedSet = item.getConnectedSet(net)`; add every
    single-net member of that set to `handledItems` (so each connected cluster contributes one
    seed item per net);
  * if `connectedSet.size() < board.connectableItemCount(net)` and the item has no ignored nets,
    **enqueue the item** (the router later routes item-per-net).
* **v2-only plane-net filter** (pipeline/BatchAutorouter.java:375-389): for nets containing a
  ConductionArea (copper pour), items whose connected set already contains the pour are skipped
  at selection time (they'd immediately return CONNECTED_TO_PLANE and waste time).
* v2 reuses two instance collections (`reusableAutorouteItemList` ArrayList,
  `reusableHandledItems` TreeSet) across passes to cut churn (lines 71-74, 347-348).

Within the pass the items are processed **in selection order** (single-thread path). The dead
multi-thread path shuffles per-thread copies with `Random(0)` (see §9).

---

## 4. Route stage — one pass (`autoroutePass`)

v2: `AutoroutePassRunner.runSingleThread(passNo)` (pipeline/AutoroutePassRunner.java:151-336);
v19: `BatchAutorouter.autoroute_pass` (BatchAutorouter.java:330-578). Same skeleton:

```
items = getAutorouteItems(board)
if items.isEmpty(): airLine=null; return false        # <- "board complete" signal
for item in items:
    for i in 0 .. item.netCount()-1:
        stop checks (user stop; maxItems limit: v19 settings.getMaxItems(),
                     v2 settings.maxItems default MAX_INT -> requestStop when totalItemsRouted >= max)
        board.startMarkingChangedArea()
        result = autorouteItem(item, net_i, rippedItemList, rippedItemCosts, passNo)
        classify:
          ROUTED                        -> routed++
          ALREADY_CONNECTED / NO_UNCONNECTED_NETS / CONNECTED_TO_PLANE -> v2: skipped++
                                          (v19 counts ALREADY_CONNECTED as routed++)
          else (FAILED / INSERT_ERROR)  -> notRouted++; v2: board.failureLog.recordFailure(...)
        rippedItemCount += rippedItemList.size()
# end-of-pass tail cleanup:
removeTails(removeUnconnectedVias ? NONE : FANOUT_VIA)
    = board.removeTraceTails(-1 /*all nets*/, stopOption); board.optChangedArea(..., 1000ms)
return v19: true (always, caught-exception -> false)
       v2:  routed > 0 || notRouted > 0     # false when nothing was attempted
```

Key per-connection parameters (`autorouteItem` → v2 `AutorouteConnectionRouter.route`,
pipeline/AutorouteConnectionRouter.java:30-160; v19 BatchAutorouter.autoroute_item:593-690):

* **Ripup cost escalation: `ripupCosts = startRipupCosts * passNo`** (linear; passNo is 1-based).
  Default startRipupCosts = **100**, so pass 1: 100, pass 2: 200, … `ripupAllowed = true`.
* **Via costs**: `viaCosts` default **50**; nets containing a plane use `planeViaCosts` default
  **5** (encourage short stubs into the pour).
* **Per-connection time limit: `min(100000 * 2^(passNo-1), Integer.MAX_VALUE)` ms** —
  100 s doubling each pass.
* Direction of search: for plane nets route **connected → unconnected** (start from what's
  already joined to the plane); otherwise **unconnected → connected**. If the connected set
  already contains a ConductionArea → CONNECTED_TO_PLANE (skip).
* Airline (GUI progress line) = min-distance pair of DrillItem centers between start and dest
  sets (v2 AutorouteAirlineCalculator.calculateAirline; O(|A|·|B|) with squared distances).
* Engine init: `board.initAutoroute(net, clearanceClass, thread, timeLimit,
  retainAutorouteDatabase)`; **retainAutorouteDatabase is `false` in production** — v2 only
  enables it under `-Dfreerouting.benchmark.retain_autoroute_database`
  (pipeline/BatchAutorouter.java:63-64, 154: "Retained state is deliberately opt-in for bounded
  benchmark experiments only"). I.e. the expansion-room database is rebuilt per connection.
* On ROUTED: `optChangedArea` pull-tight pass, accuracy = `tracePullTightAccuracy`
  (default **500**; v19 uses settings' pull-tight accuracy), time limit 1000 ms.

v2-only post-processing per connection:

* **Necked retry** (AutorouteConnectionRouter.retryConnectionNecked:162-241): if FAILED or
  INSERT_ERROR and `settings.neckWidthUm > 0` (default 0 = off), rebuild an AutorouteControl
  whose per-layer `traceHalfWidth` is clamped to `neckWidth/2` (preserving compensation deltas)
  and retry the same connection once at the narrower width.
* **Strict DRC** (BatchAutorouter.enforceStrictDrc:305-329 + applyStrictDrcAfterRoute):
  if `settings.strictDrc` (default false), snapshot the board (`serialize(false)` → byte[])
  before routing; after a ROUTED result, if any newly-inserted trace/via (id > pre-route max id)
  has a clearance violation, remove all new items, deserialize-restore the snapshot, and report
  FAILED so later passes (higher ripup budget) retry.

### 4.1 What happens to failed items

* There is **no retry inside a pass** and no per-item backoff in v19: a failed connection simply
  stays unrouted; the *next pass* re-derives the work list from scratch (the item still has
  `connectedSet < netItemCount`) and retries with 2× time budget and +startRipupCosts ripup.
* v2 records every failure in `board.failureLog` (`RoutingFailureLog`) with pass number, state,
  and details; the single-threaded pass runner only uses it for logging verbosity
  (log when ≤5 items remain or failureCount ≥ 3, AutoroutePassRunner.java:273). The (dead)
  multi-thread route path additionally consults `failureLog.shouldSkip(item)` to skip items over
  a failure threshold (BatchAutorouterThread.java:324).
* Escalation across passes is thus purely: linear ripup-cost growth + exponential time budget +
  (v2) board-restore to an earlier better state (§5.2).

### 4.2 Per-pass accounting

Both versions time the pass, then record
`PerformanceProfiler.recordPass(passNo, incompleteCount, passDurationMs, startRipupCosts*passNo)`.
Incomplete count: v19 = `new RatsNest(board).incomplete_count()`; v2 =
`new DesignRulesChecker(board, null).calculateAllIncompletes().getIncompleteCount()`.
v19 builds a **new RatsNest after every routed item** for trace logging
(BatchAutorouter.java:451 — unconditional, very hot); v2 gates the per-item DRC behind
`FRLogger.isTraceEnabled()` and refreshes progress statistics only every
`PROGRESS_STATISTICS_ITEM_INTERVAL = 10` items and fires GUI events at most every 250 ms
(`shouldFireBoardUpdate`, pipeline/BatchAutorouter.java:335-343).

---

## 5. Route stage — the batch loop (pass scheduling and stop conditions)

### 5.1 v19 `autoroute_passes` (BatchAutorouter.java:161-323)

```
loop while still_unrouted_items && !interrupted:
  if board hash already seen (already_checked_board_hashes)  -> stop (endless-loop guard)
  curr_pass_no = settings.get_start_pass_no()
  if curr_pass_no > settings.get_stop_pass_no()              -> stop
  boardBefore = board.clone()                                 # full deep copy for diffing!
  still_unrouted_items = autoroute_pass(curr_pass_no, true)
  score-stagnation: if finalScore <= bestScore + 1e-4 for 20 consecutive passes -> stop
  diff-stagnation:  diff = board.diff_traces(boardBefore); keep sliding window (init 20);
                    when window full and avg(diff) < 20.0 -> warn, grow window by +20;
                    if window size exceeds 200 -> stop
  optional save_intermediate_stage_file()
  if still unrouted && !interrupted: settings.increment_pass_no()
# post: if completed && !remove_unconnected_vias: remove_tails(NONE)  (fanout cleanup)
return !interrupted
```

Pass numbering is persisted in the settings (`start_pass_no`), so restarting the autorouter
continues escalating ripup costs. Exit criteria: complete (`autoroute_pass` returned false),
user stop, pass-number cap, board-hash repeat, 20-pass score stagnation, or diff shrinkage.

### 5.2 v2 `AutorouteBatchLoop.run()` (pipeline/AutorouteBatchLoop.java:37-588)

Constants (pipeline/BatchAutorouter.java:38-60):

| constant | value | meaning |
|---|---|---|
| `STOP_AT_PASS_MINIMUM` | 8 | min passes before any restore/stagnation check may fire |
| `STOP_AT_PASS_MODULO` | 4 | restore check runs on passes ≡ 0 (mod 4), pass ≥ 8 |
| `STAGNATION_PASS_LIMIT` | 10 | consecutive no-improvement passes before abort |
| `STAGNATION_SCORE_THRESHOLD` | 0.5 | min score gain (0–1000 scale) counted as improvement |
| `FANOUT_RECOVERY_STAGNATION_PASSES` | 3 | plateau passes before one-time fanout-tail cleanup |
| `MAXIMUM_TRIES_ON_THE_SAME_BOARD` | 3 | max restores of any single history snapshot |
| `BOARD_RANK_LIMIT` | 30 (= `BoardHistory.MAX_HISTORY_SIZE`) | restored board's rank cap |

Flow:

1. Guard: at least one active signal layer, else CANCELLED + IllegalArgumentException.
2. Fire STARTED; record `initialUnroutedCount`; create `BoardHistory bh` (score-ranked snapshot
   store, cap 30 serialized boards, §5.3).
3. **Fanout pre-pass** if enabled and the board has SMD pins (§2), incl. progress events,
   timeout, per-stage CPU/alloc metrics.
4. Pass loop `while continueAutorouting && !stopRequested`:
   * timeout (`job.state == TIMED_OUT`) → request stop.
   * `maxPasses` (default 9999) exceeded → stop. `job.setCurrentPass(currentPass)`; fire RUNNING.
   * `bh.add(board)` — snapshot the pre-pass board (dedup by hash, evict worst when full).
   * `continueAutorouting = autoroutePass(currentPass)` (§4; **single-threaded — see §9**).
   * **Board-restore check** (lines 298-344): on passes ≥ 8 with pass % 4 == 0 (or when stop was
     requested): if `bh.getMaxScore() > scoreAfter` (strict >; comment explains ">=" caused
     unbounded loops on all-equal scores), restore the best history board whose
     `restoreCount <= 3` (incrementing it); if none, or its rank > 30 → stop. On restore:
     reset the pass-local stagnation counter and `lastBestScore`, clear the same-hash set.
     *The v19 "same board hash → stop" check exists but is commented out* (lines 258-266:
     "ripup budgets and random seeds change per-pass, making progress possible").
   * Log pass completion with score; per-net incomplete breakdown at trace level; optional
     board-snapshot event when `saveIntermediateStages`.
   * **Stagnation detection** (only when pass ≥ 8 and router still active):
     * pass-local: `scoreAfter > lastBestScore + 0.5` resets counter, else counter++.
       - **One-time fanout recovery**: with fanout enabled, incompletes > 0, and counter ≥ 3,
         run `removeTails(NONE)` (rips fanout vias/tails), recompute score, reset counter,
         mark applied, clear hash set (lines 435-454).
       - counter ≥ 10 → log the per-net unrouted report (`AutorouteUnroutedReport.build`) and
         stop.
     * global (immune to restores): track `globalBestScore` / `passOfBestScore`; if
       `currentPass - passOfBestScore >= 10` → stop with report.
     * If instead the board is fully routed AND score > 0.5, reset the pass-local counter
       (score-0 fully-routed boards deliberately do NOT reset — lines 509-517).
   * `currentPass++` when continuing.
5. **Final-board guarantee** (lines 525-550): if `bh.getMaxScore() > score(currentBoard)`,
   restore the best-ever board (`bh.restoreBestBoard()`) as the final result.
6. `job.board = router.board` (line 552 — **the only place the routed board is published to the
   job**), optional final `removeTails(NONE)` when complete without fanout vias, `bh.clear()`,
   fire FINISHED / CANCELLED / TIMED_OUT.

Return `!stopAutoRouterRequested` ("completed").

`autoroutePassesForOptimizingItem` (v2 pipeline/BatchAutorouter.java:245-281; v19
BatchAutorouter.autoroute_passes_for_optimizing_item:114-144) is the mini batch loop used by the
optimizer: run up to `maxPassCount` passes with a FIXED ripup cost (note: the connection-level
`ripupCosts = startRipupCosts * passNo` formula uses the instance's startRipupCosts = the
optimizer-provided cost, so it still climbs linearly within those ≤ 6 passes), then
`removeTails(NONE)`. Returns the pass count needed (or max+1 if incomplete).

### 5.3 `BoardHistory` (v2-only; autoroute/BoardHistory.java)

* Entry = `{byte[] serializedBoard, String hash, float score, int restoreCount}`; score computed
  once at insert (`new BoardStatistics(board).getNormalizedScore(...)`).
* `add`: dedup by hash; at cap (30) compute new board's score first and only evict the
  worst entry if strictly improved; otherwise skip serialization entirely.
* `restoreBoard(maxRestores)`: sort by score desc, return the first entry with
  `restoreCount <= maxRestores` (incrementing it), materialized via
  `BasicBoard.deserialize(byte[])` (Java object deserialization — expensive).
  `restoreBoard(<=0)` means "unlimited restores" → `restoreBestBoard()` = best regardless.
* Thread-safe via `synchronized` + a read-write lock (belt and braces; also used by the dead MT
  route path).

---

## 6. Scoring

### 6.1 v2 (authoritative for 2.3.0 logs): `BoardStatistics.getNormalizedScore`

core/scoring/BoardStatistics.java:600-640, weights from
settings/sources/DefaultSettings.java:

```
maxConnections = Σ over nets with ≥1 item of max(0, endpointCount - 1)
                 where endpoints = Pins + ConductionAreas only   (drc/DesignRulesChecker.java:567)
maximumScore = maxConnections * unroutedNetPenalty                       # 5_000_000 per connection
penalties    = incompleteCount      * unroutedNetPenalty                 # 5_000_000
             + clearanceViolations  * clearanceViolationPenalty          # 1_000_000
             + totalBends           * bendPenalty                        # 10
costs        = traceLengthMm * defaultPreferredDirectionTraceCost        # 1.0 / mm  (mm-normalized!)
             + viaCount * viaCosts                                       # 50 (plane vias not special-cased here)
score            = maximumScore - penalties - costs
normalizedScore  = max(0, score / maximumScore) * 1000        # 0..1000; 0 if maximumScore <= 0
```

Trace length is normalized to millimetres via
`Unit.scale(1, boardUnit, MM) / resolution` (BoardStatistics.java:194-203) — the comment
documents that un-normalized KiCad nm units used to force the score to 0. The guard returning 0
for `maximumScore <= 0` exists to keep stagnation counters advancing (NaN comparisons are
always false).

### 6.2 v19 (calculateScore in BatchAutorouter.java:757-789 and BatchOptRoute.java:122-154)

Same shape, different constants: `maxConnections = Σ max(0, connectableItemCount(net) - 1)`
(ALL connectable items, not just pins/pours), penalties = incompletes × **1_000_000** +
violations × **1_000** + bends × 10; costs = traceLengthMm × 1 + viaCount × viaCosts(settings).
Normalized identically to 0..1000. Violations are deduplicated by
`minId-maxId-layer` string keys over `item.clearance_violations()` of every item (O(board)).

Log format everywhere: `FRLogger.formatScore(score, incompleteCount, violations)`.

---

## 7. Optimizer stage — single-threaded (production path)

### 7.1 Item enumeration: `ReadSortedRouteItems`

Identical algorithm in v19 (BatchOptRoute.java:473-572) and v2 (pipeline/BatchOptimizer.java:563-659).
It is a **stateful "next item strictly after cursor" scan re-run from scratch each call**
(because the board mutates between items):

* Order key: ascending (x, y, layer) of an item's *representative corner*:
  vias → center + first layer; traces → the lexicographically larger endpoint
  (`max(firstCorner, lastCorner)` by (x,y)), layer.
* Two full scans of `itemList` per `next()` call: first all non-user-fixed **vias**, then all
  non-shove-fixed **traces** — so at equal position vias win. Traces directly contacting a
  non-fixed via are skipped (the via will cover them).
* Cursor `(minItemCoor, minItemLayer)` advances to the returned key. Complexity: O(N) scans ×
  N items = **O(N²) per optimizer pass** — a deliberate simplicity/correctness tradeoff upstream;
  a Rust rewrite can replace this with an ordered index invalidated per mutation.

### 7.2 Per-item optimization: `optRouteItem`

v19 `opt_route_item` (BatchOptRoute.java:310-426); v2 `optRouteItem`
(pipeline/BatchOptimizer.java:395-514, with `disableSnapshots` param). Steps:

1. `rippedItems = {item}`; for a trace, also both end-contact groups if each group
   `containsOnlyUnfixedTraces` (fork traces missed by the scan's endpoint matching).
2. `rippedConnections = ∪ item.getConnectionItems(StopConnectionOption.NONE)` — the full
   electrical connection(s). If any member is user-fixed → return unimproved result.
3. If snapshots enabled: `board.generateSnapshot()` (undo point).
4. `board.removeItems(rippedConnections)`; `combineTraces(net)` per net of the item.
5. Ripup costs for the re-route:
   * base = `startRipupCosts` (100);
   * `useIncreasedRipupCosts` (true for the first pass(es)) → ×
     `additionalRipupCostFactorAtStart` = **10** (v19 constant
     `ADDITIONAL_RIPUP_COST_FACTOR_AT_START = 10`);
   * if the item is a Trace → × `traceRipupCostFactor` = **0.6**, rounded
     ("taking less ripup costs seems to produce better results").
6. `BatchAutorouter.autoroutePassesForOptimizingItem(...)` with
   `MAX_AUTOROUTE_PASSES` = **6** (v2 `optimizer.maxAutoroutePasses`, default 6),
   preferred directions toggled by the caller.
7. Build `ItemRouteResult(itemId, viaCountBefore/After, weightedTraceLenBefore/After,
   incompleteBefore/After)` and set `improved &= !stopRequested`.
8. If improved: keep (pop snapshot); update the running `minCumulativeTraceLength`
   (v19 has extra logic: only adopt the new length outright when incompletes or vias strictly
   improved, else `min(old, new)` — "catch unexpected increase … by removing acid traps").
   If not improved: `board.undo(null)` restores the snapshot.

**Improvement predicate** (`ItemRouteResult` ctor, identical v19/v2): lexicographic —
fewer incompletes ≻ fewer vias ≻ shorter (weighted) trace length; ties = not improved.
`improvement_percentage = 1 - ((viaAfter/viaBefore + lenAfter/lenBefore)/2)` — note v19 uses
**integer division** for `via_count_after / via_count_before` (ItemRouteResult.java:55-62), a
long-standing bug that quantizes the via ratio to 0 or 1; v2 casts to float
(BatchOptimizer.java:345). `compareTo` orders by (incompleteAfter, viaAfter, lengthAfter)
ascending; `improvedOver(r) = compareTo(r) < 0`.

The *weighted* trace length metric (`calc_weighted_trace_length`, BatchOptRoute.java:66-95 /
`BoardStatistics.traces.totalWeightedLength`): Σ over UNFIXED and SHOVE_FIXED traces of
`length * (halfWidth + clearance(traceClass, defaultClass, layer))`, halved for SHOVE_FIXED
("to produce less violations with pin exit directions").

### 7.3 Optimizer pass & stop conditions

v19 `optimize_board` (BatchOptRoute.java:157-237):

```
use_increased_ripup_costs = true; route_improved = -1
while (route_improved >= threshold || route_improved < 0) && !stopRequested:
    pass++; with_preferred_directions = (pass % 2 != 0)     # alternate "to create more variations"
    route_improved = opt_route_pass(pass, with_preferred_directions)
       # iterates ReadSortedRouteItems; tracks best (viaRatio+lenRatio)/2 improvement
       # at pass end: if use_increased_ripup_costs && route_improved == 0:
       #     use_increased_ripup_costs = false; return -1   # force another pass at low ripup
    optionally save intermediate stage when improved > threshold
```

The GUI default `optimization_improvement_threshold` flows from BoardHandling
(constructor param); pass alternation of preferred directions is the only stochastic element.

v2 `runBatchLoop` (pipeline/BatchOptimizer.java:125-272) — stop conditions, in order:

* `optimizer.maxPasses` (default **100**) reached; `optimizer.maxItems`
  (default MAX_INT) total item attempts reached; user stop; stage timeout
  (`optimizer.timeout`, default none).
* **Near-perfect early exit**: before a pass, if
  `scoreBefore * (1 + improvementThreshold) >= 1000` stop ("already close to maximum").
* Per pass, `withPreferredDirections = (pass % 2 != 0)`; run `optRoutePass`.
* After the pass: `passImprovement = (scoreAfter - scoreBefore)/scoreBefore` (score-based, not
  via/length-based like v19). If `useIncreasedRipupCosts` and no gain → drop to normal ripup
  costs and continue (improvement := -1). Else stop when
  `improvement < optimizer.optimizationImprovementThreshold` (default **0.01** = 1%).
* Inside `optRoutePass` (v2-only): abort the pass after
  `optimizer.maxConsecutiveFailures` = **50** consecutive unimproved items; per-item timeout
  and maxItems checks; progress events throttled to 1/s (`ProgressThrottler(1000)`).

Entry criterion for the whole stage: `getRunOptimizer()` and (v2) `optimizer.enabled`
default true. Note v2's optimizer, unlike the router, has **no snapshot/restore of whole boards**
— monotonic keep/undo per item guarantees the board never regresses.

---

## 8. THE MULTITHREADED OPTIMIZER

### 8.1 v19 `BatchOptRouteMT` + `OptimizeRouteTask`

Work decomposition — **one task per candidate item, one pass at a time**:

1. `opt_route_pass(passNo, withPrefDirs)` (BatchOptRouteMT.java:203-375):
   * `prepare_next_round_of_route_items()` — enumerate item ids via `ReadSortedRouteItems`
     on the *master* board, order per `ItemSelectionStrategy`:
     * `SEQUENTIAL`: scan order (forced whenever the update strategy is GLOBAL_OPTIMAL);
     * `RANDOM`: `Collections.shuffle` (unseeded!);
     * `PRIORITIZED`: items with a previous-pass `ItemRouteResult` first, ordered by a
       `PriorityQueue<ItemRouteResult>` (best `compareTo` first = most promising first),
       then never-seen items in scan order. `result_map` is cleared each round.
   * Create a fixed `ThreadPoolExecutor` of `thread_pool_size` workers.
   * For every item id, construct an `OptimizeRouteTask` and `pool.execute` it.
     **Board cloning strategy: each task's constructor runs
     `super(p_optimizer.thread, /*clone_board=*/true)` → `hdlg.deep_copy_routing_board()` —
     a full board deep-copy per task, taken eagerly at scheduling time** (OptimizeRouteTask.java:23,
     BatchOptRoute.java:49-50). The code comment "schedule just enough tasks to keep workers
     busy in order not to exhaust JVM memory" (BatchOptRouteMT.java:255-257) is aspirational —
     the loop constructs *all* tasks (and thus all clones) up front. The task then finds its
     item by **linear scan of the cloned board's item list by id** (findItemOnBoard).
   * `pool.shutdown()`; poll `awaitTermination(1 s)` in a loop, honoring stop requests with
     `shutdownNow()`.
2. Each task run: `route_result = opt_route_item(curr_item, pass_no, with_pref_dirs)` — the
   full §7.2 algorithm against its own clone (with `clone_board=true`, so no snapshots/undo:
   a losing clone is simply discarded). Overrides: `remove_ratsnest()` no-op,
   `get_ratsnest()` returns a fresh RatsNest of the clone.
3. **Result comparison / synchronization** — `synchronized boolean is_winning_candidate(task)`
   on the optimizer (BatchOptRouteMT.java:108-141): store the result in `result_map` (for
   PRIORITIZED next round); if `improved()` and (first winner or
   `improved_over(best_route_result)` by the lexicographic compareTo) → this task becomes
   `winning_candidate` (previous winner's clone is `clean()`ed: nulls board/item references to
   help GC). The `CountDownLatch task_completion_signal` is counted down but effectively
   vestigial (nothing awaits it in this code).
4. **Applying results** — `update_master_routing_board()` copies the winner's *entire cloned
   board* over the master (`hdlg.update_routing_board(winning_candidate.routing_board)`):
   * `GREEDY`: applied immediately inside `is_winning_candidate` whenever a new winner appears —
     "new tasks will copy the updated board" (only true for tasks not yet constructed; since all
     tasks are constructed up front, in practice all clones are from the pass-start board).
   * `GLOBAL_OPTIMAL`: applied once, after the pool drains, if the best result improved.
   * `HYBRID` (`p_hybrid_ratio` like "1:1"): a per-round rotation through a list of
     `num_optimal` GLOBAL_OPTIMAL entries then `num_prioritized` GREEDY entries;
     `hybrid_index` advances once per `prepare_next_round_of_route_items`. Item selection
     degrades to SEQUENTIAL during GLOBAL_OPTIMAL rounds.
5. After the pass: `route_improved = best_route_result.improvement_percentage()`; same
   increased-ripup-cost drop rule as single-thread. Outer loop = inherited
   `BatchOptRoute.optimize_board`.

**Why v19 declares it broken**: only ONE task's board is promoted per improvement, but tasks race
on independent clones of the same origin; every non-winning improvement is discarded, and a
GREEDY promotion mid-pass switches the master under tasks that hold clones of the *old* master —
their results are then compared by counts/lengths that no longer describe the promoted board.
Upstream's own release warning (v19 BatchAutorouterThread.java:49) says it "is known to
generate clearance violations", and v19 defaults `num_threads` to **1** (StartupOptions.java:30).

### 8.2 v2 `BatchOptimizerMultiThreaded` + pipeline `OptimizeRouteTask`

Direct port of the v19 design (same strategies, same synchronized winner selection, same
CountDownLatch vestige, same eager task construction), with these differences:

* Task construction: `this.board = job.board.deepCopy(); itemToOptimize = board.getItem(itemId)`
  (pipeline/OptimizeRouteTask.java:30-31) — id lookup is a board API call, not a hand-rolled scan.
* Task run: `new BatchOptimizer(this.job).optRouteItem(itemToOptimize, withPrefDirs,
  /*disableSnapshots=*/true)` — a **fresh single-thread optimizer instance per task**. Two
  consequences (both behavioral regressions vs v19):
  * the fresh instance's `minCumulativeTraceLength` is 0.0 (never set — `runBatchLoop`
    isn't called), so `ItemRouteResult.traceLengthBefore = 0` → the "shorter trace length"
    branch of `improved` can never fire; only incomplete/via reductions count
    (v19 passed `p_min_cumulative_trace_length` into the task, OptimizeRouteTask.java:22-32);
  * `useIncreasedRipupCosts` is false in the fresh instance, so MT tasks never get the ×10
    initial ripup boost.
* `replaceMasterRoutingBoardWithTheWinningCandidate` (lines 147-156) sets **`this.board`**
  (the optimizer's field) to the winner clone — but `job.board` is **never reassigned**
  (grep: only `AutorouteBatchLoop.java:552` assigns `job.board`). Since every new task deep-copies
  `job.board`, GREEDY promotions never propagate into subsequent tasks, and — worse — the final
  SES output is written from `routingJob.board` (GuiRoutingJobWorker.java:640-646), so **MT
  optimizer improvements are dropped from the saved result entirely**. Additionally, after a
  promotion, `ReadSortedRouteItems` enumerates ids from the promoted clone while tasks look
  those ids up in the stale `job.board`; missing ids yield `itemToOptimize == null` → task
  no-ops. Conclusion: **v2's MT optimizer is vestigial/broken; the production-quality path in
  2.3.0 is the single-threaded `BatchOptimizer`** (and headless jobs can only ever get that one).
* Thread pool: daemon threads named "batch-optimizer-worker" with uncaught-exception logging.
* Defaults if it *is* instantiated (GUI + flags): `boardUpdateStrategy = GREEDY`,
  `itemSelectionStrategy = PRIORITIZED`, `hybridRatio = "1:1"`,
  `maxThreads = max(1, cores-1)` (DefaultSettings.java:134-138).

### 8.3 Enums

`BoardUpdateStrategy { GREEDY, GLOBAL_OPTIMAL, HYBRID }`,
`ItemSelectionStrategy { SEQUENTIAL, RANDOM, PRIORITIZED }` — identical in both trees.

---

## 9. Is the ROUTE pass itself ever parallel in v2? — No (evidence)

* `AutorouteBatchLoop.run()` calls `autoroutePass(...)` → `passRunner.runSingleThread(passNo)`
  (pipeline/BatchAutorouter.java:419-421). This is the only call site in the batch loop.
* `BatchAutorouter.autoroutePassMultiThread(int)` (line 411) → `passRunner.runMultiThread` →
  `BatchAutorouterThread` exists, but **grep over the whole v2 tree finds zero callers** of
  `autoroutePassMultiThread` outside its own definition. It is dead code.
* The dead design, for the record (pipeline/AutoroutePassRunner.runMultiThread:40-149): a
  **racing-clones** scheme, not work-sharing — spawn `settings.maxThreads` full board deep-copies,
  each `BatchAutorouterThread` routes its OWN shuffled copy of the *entire* item list
  (`shuffle(list, router.random)` with `Random(0)`, MIN_PRIORITY daemon threads named
  "Router thread #<pass>.<A..Z>"); afterwards every finished board goes into a `BoardHistory`
  and `router.board = boardHistory.restoreBestBoard()` — best normalized score wins, all other
  work is discarded. Note the latent bug: `autorouterThread.join(TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP)`
  waits at most **1000 ms** per thread (line 94) and then reads its board anyway — a data race
  and an effective 1 s cap that alone would make this path unusable.
* Therefore, **upstream 2.3.0 routes strictly sequentially**; the only concurrency upstream ever
  exposed is (a) the broken MT optimizer (§8) and (b) this dead racing-clones route pass. Neither
  demonstrates safe intra-board parallel routing. What upstream DID prove safe: total-board
  "clone, mutate independently, compare by score, keep one" — embarrassingly parallel at the
  cost of a full board copy per unit of work, with a single synchronized compare-and-promote.

---

## 10. Call graph (batch orchestration area)

Inbound:

* v2: `RoutingJobSchedulerActionThread` (headless) and `GuiRoutingJobWorker` (GUI)
  → `RoutingPipeline.run` → `BatchAutorouter.runBatchLoop` → `AutorouteBatchLoop.run`
  → { `BatchFanout.fanoutBoard`, `AutoroutePassRunner.runSingleThread` } and
  → `BatchOptimizer.runBatchLoop` (→ `BatchOptimizerMultiThreaded.optRoutePass` when MT).
* v19: `InteractiveActionThread.get_batch_autorouter_instance` → `BatchAutorouterThread
  .thread_action` → { `BatchFanout.fanout_board`, `BatchAutorouter.autoroute_passes`,
  `BatchOptRoute.optimize_board` }.

Outbound (dependencies this layer drives):

* `RoutingBoard`: `initAutoroute` (engine per net), `fanout`, `optChangedArea` (pull-tight),
  `removeTraceTails`, `startMarkingChangedArea`, `removeItems`, `combineTraces`,
  `generateSnapshot`/`popSnapshot`/`undo`, `deepCopy`, `serialize`/`deserialize`, `getHash`,
  `getStatistics`, `connectableItemCount`, `getConnectableItems`.
* `AutorouteEngine.autorouteConnection(startSet, destSet, control, rippedItems, ripupCosts)` —
  the maze router; consumes `AutorouteControl` (via costs, trace costs per layer, ripupAllowed,
  ripupCosts, isFanout, fanout pin metadata, neck widths).
* Metrics/eventing: `PerformanceProfiler` (recordConfiguration / recordPass / start/end),
  v2 event listeners (`BoardUpdatedEvent`, `TaskStateChangedEvent`, `BoardSnapshotEvent` via
  `NamedAlgorithm`), `DesignRulesChecker` (incomplete counts, airlines) / v19 `RatsNest`.
* `ItemAutorouteInfo` (autoroute/ItemAutorouteInfo.java) is per-item scratch owned by the maze
  layer (start/dest flag, cached `Connection`, lazily-built `ObstacleExpansionRoom[]` per tree
  shape, `reset_doors` between connections) — listed here because the orchestrator's
  "fresh engine per connection" policy is what forces this cache to be rebuilt constantly.

---

## 11. Rust design implications

### Keep semantically (proven behavior, bake into the spec)

1. **Stage order** fanout → route passes → optimize, with fanout folded into the route stage
   entry (v2) and the maxPasses=0 trick replaced by an explicit `fanout_only` mode.
2. **Linear ripup escalation** `cost = start_ripup_costs (100) * pass_no` for routing;
   `(pass+1)` variant for fanout; optimizer policy `100 ×10-at-start ×0.6-for-traces`,
   inner reroute capped at 6 passes. Exponential per-connection time budget
   `100 s * 2^(pass-1)` (make it configurable; wall-clock budgets are a poor fit for a much
   faster core — consider expansion-count budgets instead).
3. **v2 scoring** exactly: per-connection 5 M, violation 1 M, bend 10, trace 1/mm, via 50
   (plane via 5), normalized to 0..1000, `maxConnections` counted over pins+pours only, and the
   mm normalization (BoardStatistics.java:194).
4. **Stop machinery constants**: min 8 passes, restore check every 4, stagnation threshold 0.5
   over 10 passes (pass-local AND global trackers), 3 restores per snapshot, history cap 30,
   one-time fanout-tail recovery after 3 flat passes, final "best board ever" restore.
5. **Optimizer semantics**: (x, y, layer) sweep order with via-priority; connection ripout with
   fork-trace absorption; lexicographic improvement (incompletes, vias, weighted length);
   keep/rollback per item; alternate preferred-directions per pass; v2 exits
   (1% score threshold, 50 consecutive failures, near-1000 early-out).
6. **Fanout semantics**: component ordering by pin count, configurable pin order (default
   outer_first), maze-driven implicit via placement with "complete at first drill-to-drill" and
   the v2 min/max escape annulus (2.5–4.5 mm), fanout-via ripup protection factor,
   `remove_tails(FANOUT_VIA)` vs `NONE` distinction so fanout stubs survive until completion.

### Restructure

1. **Board snapshots**: upstream serializes whole boards (Java serialization) for BoardHistory,
   strict-DRC rollback, and clone-per-task. In Rust, make the board a persistent/arena structure:
   copy-on-write item store (or generation-stamped undo log) so that "snapshot" is O(1) and
   "restore" is a pointer swap. This turns BoardHistory (30 × multi-MB byte[]s + deserialization)
   and strict-DRC (serialize per connection!, AutorouteConnectionRouter.java:84-85) into free
   operations, and makes the racing-clones idea actually cheap.
2. **Item selection**: replace the O(N) itemList scans (`getAutorouteItems` every pass,
   `ReadSortedRouteItems.next()` O(N) per item → O(N²) per optimizer pass,
   v19 `findItemOnBoard` O(N) per task) with: a dirty-net queue for route work (only nets whose
   connectivity changed need re-examination) and a BTreeMap keyed by (x, y, layer, id) for the
   optimizer sweep, invalidated incrementally.
3. **Incomplete counts / score**: upstream recomputes full DRC/RatsNest constantly
   (v19 per routed item — BatchAutorouter.java:451; v2 per progress tick and 3× per pass).
   Maintain incremental connectivity (union-find per net over items, updated on insert/remove)
   so incompleteCount and score are O(changed) not O(board).
4. **Parallelism strategy** (what upstream's evidence supports):
   * Upstream proved NOTHING about intra-board shared-state parallel routing — both parallel
     paths are clone-based, and the shared-master variant (MT optimizer GREEDY) is the one
     documented as producing clearance violations. Do not copy that design.
   * Upstream's clone-and-race pattern IS sound (score-compare + single promote) and maps
     cleanly to Rust: with a CoW board, "clone per worker" is cheap, so both the dead v2 route
     racer and the GLOBAL_OPTIMAL optimizer become viable as *portfolio parallelism*
     (different shuffles/seeds racing, best board wins) with a `std::sync::mpsc` /
     rayon-join reduction instead of `synchronized` + polling `awaitTermination(1 s)`.
   * For true work-sharing, the safe unit suggested by the data layout is the *net/connection*
     with spatial disjointness: route connections whose expansion regions don't overlap
     concurrently, serialize conflicts. That is new work, not an upstream port — treat upstream
     behavior (sequential order, deterministic results given `Random(0)`) as the golden
     reference for a `--deterministic` mode.
5. **Events/metrics**: v2 throttles UI events to 250 ms/1 s (`shouldFireBoardUpdate`,
   `ProgressThrottler`); replicate as a sampling channel, never compute statistics for a dropped
   event (upstream still pays `new BoardStatistics(...)` every 10 items regardless of whether an
   event fires — invert that).
6. Replace stringly-typed knobs (`pinSortingOrder: String`, `hybridRatio: "1:1"`,
   timeout strings) with enums/Durations at the config boundary.

### Java-isms observed (churn/hot-path evidence)

* **Per-item full-board rats-nest**: v19 BatchAutorouter.java:451 `new RatsNest(routing_board, …)`
  inside the per-item loop (unconditional), plus again at 519 per pass; BatchOptRoute
  remove/get ratsnest pair per optimized item (BatchOptRoute.java:325-327).
* **Serialize-heavy control flow**: BoardHistory entries are `board.serialize(false)` byte[]s
  (BoardHistory.java:196) restored via Java deserialization (BoardHistory.java:148);
  strict-DRC snapshots serialize the whole board before *every* connection when enabled
  (AutorouteConnectionRouter.java:84-85, 251).
* **Deep copy per pass for diffing**: v19 BatchAutorouter.java:216 `routing_board.clone()` at
  the top of every pass purely to feed `diff_traces` stagnation heuristics.
* **Clone-per-task MT optimizer**: v19 OptimizeRouteTask.java:23 (via BatchOptRoute.java:50)
  and v2 pipeline/OptimizeRouteTask.java:30 — one full board deep-copy per item task, all
  constructed eagerly (v19 BatchOptRouteMT.java:254-275; v2 BatchOptimizerMultiThreaded.java:251-261),
  contradicting their own "just enough tasks" comments; `clean()` nulls fields to beg the GC
  (OptimizeRouteTask.java:113-119).
* **O(N) id lookups**: v19 OptimizeRouteTask.findItemOnBoard:35-53 linear-scans the item list.
* **Allocation-chatty inner loops**: fresh `TreeSet`/`LinkedHashMap` per routed connection
  (AutoroutePassRunner.java:225-226; v19 BatchAutorouter.java:410-411); string-key violation
  dedup `minId + "-" + maxId + "-" + layer` (v19 BatchAutorouter.java:749); log-string
  construction on hot paths even when disabled in v19 (v2 partially gates with
  `FRLogger.isTraceEnabled()`, AutoroutePassRunner.java:252).
* **Integer-division bug** in v19 improvement percentage (ItemRouteResult.java:55-62) — fixed in
  v2 (BatchOptimizer.java:345); pick the v2 semantics.
* **ThreadMXBean sampling** for CPU/alloc metrics scattered through orchestration
  (BatchFanout, BatchOptimizer, AutorouteRuntimeMetrics, BatchAutorouterThread.captureStats) —
  in Rust use a metrics facade fed by explicit counters, not thread introspection.
* **Debug leftovers shipped in release paths**: hardcoded net-94 dump
  (AutoroutePassRunner.java:256-258, logNet94Items) and "U27-" pin-name fanout diagnostics
  (v19 BatchFanout.java:195-203) — exclude from the port; they are parity-testing scaffolding.
* **Vestigial synchronization**: `CountDownLatch task_completion_signal` counted down but never
  awaited (both trees); `awaitTermination(1 s)` polling loops for progress logging;
  `join(1000)` race in the dead MT route pass (AutoroutePassRunner.java:94).
