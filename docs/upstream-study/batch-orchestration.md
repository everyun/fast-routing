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

---

## Release-jar conformance (2.3.0 @2d4de019)

Audited against the true source of the shipped jar, `/tmp/fr-java-release/src/main/java/app/freerouting`
(jar `/tmp/frbase/freerouting-2.3.0.jar`, `Build-Revision: 2d4de019aa89e9fa3dc1dc44e09bf509760cafc1`,
`Build-Date: 2026-08-07`). All file:line references below are in that tree unless prefixed otherwise.
Constants marked *(jar-verified)* were additionally read back with
`javap -p -c -constants` from the shipped `.class` files.

**Headline correction.** The body of this spec was written against `/tmp/fr-java/src` (git master),
whose `autoroute/pipeline/` package is **not in the release**. `unzip -l freerouting-2.3.0.jar |
grep pipeline` returns only shaded netty classes — there is no `app/freerouting/autoroute/pipeline/`.
`RoutingPipeline`, `AutorouteBatchLoop`, `AutoroutePassRunner`, `AutorouteConnectionRouter`,
`AutorouteAirlineCalculator`, `AutorouteRuntimeMetrics`, `AutorouteUnroutedReport`, `BatchOptRoute`,
`BatchOptRouteMT` and `GuiRoutingJobWorker` do not exist in 2.3.0. The release is a **flat
`autoroute/` package** in which the pipeline's responsibilities are folded back into
`autoroute/BatchAutorouter.java` (1764 L) and the two `thread_action()` entry points.

### 1. What the release actually executes, per subsystem

#### 1.1 Entry-point call chains (verified by grep over the whole release tree)

**Headless / API jobs** — `management/RoutingJobSchedulerActionThread.thread_action()` (`:38`):

```
:96   if (job.routerSettings.getRunRouter() && (maxPasses == null || maxPasses >= 0))
:100-108   NamedAlgorithm router; String algorithm = job.routerSettings.algorithm;
           if (!ALGORITHM_CURRENT.equals(algorithm)) job.logInfo("Unknown router algorithm ...");
:106       // literal comment in the source: "Always use standard BatchAutorouter"
:107       router = new BatchAutorouter(job);
:118       batchRouter.runBatchLoop();
:163  else if (job.routerSettings.isFanoutEnabled())      // fanout-only mode
:168-170     maxPasses = 0; new BatchAutorouter(job).runBatchLoop();
:179  if (job.routerSettings.getRunOptimizer())
:182     BatchOptimizer optimizer = new BatchOptimizer(job);   // ALWAYS single-threaded
:189     optimizer.runBatchLoop();
```

**GUI jobs** — `interactive/AutorouterAndRouteOptimizerThread` (ctor `:199`, `thread_action()` `:451`):

```
ctor :204-218  algorithm == RouterSettings.ALGORITHM_V19 ("freerouting-router-v19")
                   -> batchAutorouter = new BatchAutorouterV19(job)      // :209
               else (unknown ids normalised to ALGORITHM_CURRENT with a warning, :212-216)
                   -> batchAutorouter = new BatchAutorouter(job)         // :217
ctor :272      if (routerSettings.optimizer.enabled)                     // raw Boolean unboxing
     :273-274      (!featureFlags.multiThreading || optimizer.maxThreads == 1)
                       -> batchOptimizer = new BatchOptimizer(job)
     :312-314      (featureFlags.multiThreading && optimizer.maxThreads > 1)
                       -> batchOptimizer = new BatchOptimizerMultiThreaded(job)
thread_action
     :492      isRouterEnabled = getRunRouter() && (maxPasses == null || maxPasses >= 0)
     :510-517  runBatchLoop() on whichever router instance
     :518-529  else if isFanoutEnabled(): maxPasses := 0; runBatchLoop(); restore
     :533      boardManager.replaceRoutingBoard(routingJob.board)
     :592-607  num_threads = boardManager.get_num_threads();
               if (num_threads > 0 && optimizer.enabled) { if (num_threads > 1) logWarning(MT-broken);
                  if (getRunOptimizer() && !isStopRequested()) batchOptimizer.runBatchLoop(); }
     :639-648  SES written from boardManager.saveAsSpecctraSessionSes(...)
```

**Which Batch\* class ships and runs, definitively:**

| context | router class | selectable? |
|---|---|---|
| headless (`-de/-do`, REST API, MCP) | **`autoroute/BatchAutorouter`, always** | No. `RoutingJobSchedulerActionThread:106` hardcodes it; a non-default `algorithm` only produces a log line at `:104`. `BatchAutorouterV19` is **unreachable headless**. |
| GUI | `BatchAutorouter` (default) or `BatchAutorouterV19` | Yes — `router.algorithm = "freerouting-router-v19"` (`settings/RouterSettings.java:15`). |

`BatchAutorouterV19` (507 L) ships in the jar but is a **thin re-implementation, not the real v1.9
loop**: `runBatchLoop()` (`:154-251`) has *no* board-hash guard, *no* `board.clone()`/`diff_traces`
stagnation window, *no* 20-pass score-stagnation rule, *no* `BoardHistory`, *no* `start_pass_no`
/`stop_pass_no` settings, and **no fanout stage at all** (grep: zero `BatchFanout` references in
that file). Its only stop conditions are `maxPasses`, user stop, `job.state == TIMED_OUT`, and
`autoroute_pass()` returning false. Its ctor forces `remove_unconnected_vias = true` (`:71`) and
clamps `maxThreads` to 1 (`:85-88`). It scores with the same `BoardStatistics.getNormalizedScore`
as the v2 router (`:207`) — the v19 scoring constants of §6.2 are **nowhere in the release**.

#### 1.2 Stage sequencing in the release

There is no orchestrator object. Stage order lives in two places:

* **fanout is inside the router**: `BatchAutorouter.runBatchLoop()` runs the fanout pre-pass at
  `:798-891`, gated on `settings.isFanoutEnabled() && !board.get_smd_pins().isEmpty()`, *before*
  the pass loop at `:915`. This matches the spec's §1.1 description of the v2 pipeline shape.
* **route → optimize** is sequenced by `thread_action()` in each of the two entry points above.

The "`maxPasses := 0` trick" for fanout-only exists in both entry points (`RoutingJobSchedulerActionThread:168`,
`AutorouterAndRouteOptimizerThread:521`) but **is only correct when the router was disabled via
`router.enabled=false`**. `BatchAutorouter.runBatchLoop:894` recomputes
`isRouterEnabled = getRunRouter() && (maxPasses == null || maxPasses >= 0)`, and the pass cap at
`:931` requires `maxPasses > 0`. So entering fanout-only mode via a **negative** `max_passes`
(the other way to fail the `:96`/`:492` guard) sets `fanoutOnly.maxPasses = 0` (the release literally
assigns 0 to the maxPasses field; renamed here so the cross-spec lint can tell this fanout-only
sentinel apart from the real defaults in §2), re-enables the router inside
`runBatchLoop`, and then runs the pass loop **with no pass cap at all** until stagnation fires.
Port note: model fanout-only as an explicit mode, never as a magic pass count (this spec's §11.1
already recommends that; the release is the cautionary example).

### 2. Claims table — spec vs release source

`=` means the release matches the spec exactly. Line refs are release-tree.

#### 2.1 Structure / orchestration

| # | spec claim | release | evidence |
|---|---|---|---|
| 1 | Batch classes live in `autoroute/pipeline/`; entered from `RoutingJobSchedulerActionThread:99` → `RoutingPipeline.createForHeadless` and `GuiRoutingJobWorker:212` → `createForGui` | **WRONG for the release.** No `pipeline` package, no `RoutingPipeline`, no `GuiRoutingJobWorker`. Entry chains are §1.1 above. | `unzip -l` (no `app/freerouting/autoroute/pipeline/*`); `management/RoutingJobSchedulerActionThread.java:96-192`; `interactive/AutorouterAndRouteOptimizerThread.java:451-701` |
| 2 | `BatchOptimizer.createForGui` / `createForHeadless` factories, `RoutingPipeline.normalizeRouterAlgorithm`, `BatchOptimizer.normalizeAlgorithm` | **ABSENT as named APIs.** Construction and id-normalisation are inline. | `AutorouterAndRouteOptimizerThread.java:211-216` (router id), `:316-321` (optimizer id), `RoutingJobSchedulerActionThread.java:103-105` |
| 3 | Headless: ALWAYS single-threaded `BatchOptimizer` | **=** | `RoutingJobSchedulerActionThread.java:182`; `BatchOptimizerMultiThreaded` has exactly one construction site tree-wide: `AutorouterAndRouteOptimizerThread.java:314` |
| 4 | GUI MT optimizer iff `featureFlags.multiThreading` (default `true`) AND `optimizer.maxThreads > 1` (default `max(1, cores-1)`) | **=** | `AutorouterAndRouteOptimizerThread.java:312`; `settings/FeatureFlagsSettings.java:9`; `settings/sources/DefaultSettings.java:130` |
| 5 | (implied) the optimizer stage gate is `optimizer.enabled` | **partly.** GUI gates the *stage* on `boardManager.get_num_threads() > 0 && optimizer.enabled` (`:593`) — i.e. on the **router's** `max_threads`, not the optimizer's — while the *class choice* used `optimizer.maxThreads`. Two different knobs. | `AutorouterAndRouteOptimizerThread.java:273/312` vs `:593`; `interactive/GuiBoardManager.java:3287` |

#### 2.2 `getRunOptimizer` null-handling and `--router.optimizer.enabled=true`

| # | claim | release |
|---|---|---|
| 6 | `getRunOptimizer()` / `optimizer.enabled` default true | **Two different defaults, and they disagree.** `RouterSettings.getRunOptimizer()` (`settings/RouterSettings.java:492-494`) is **fail-closed**: `return optimizer != null && optimizer.enabled != null ? optimizer.enabled : false;` — a `null` `enabled` means **do not optimize**. Contrast `getRunRouter()` (`:484`, fail-**open**, `null → true`) and `getRunFanout()` (`:503`, fail-open but dead — its only caller is `gui/WindowAutorouteParameter.java:628`; the live gate is `isFanoutEnabled()` at `:510`, which is fail-**closed**). |
| 7 | — | In practice `optimizer.enabled` is never null on a job, because every job's settings come from `SettingsMerger.merge()` (`Freerouting.java:118`, `management/RoutingJobScheduler.java:107`, `gui/GuiManager.java:118`) whose priority-0 base is `DefaultSettings`, which sets `optimizer.enabled = true` (`settings/sources/DefaultSettings.java:126`). So **the optimizer runs by default, headless and GUI.** |
| 8 | — | `--router.optimizer.enabled=true` is parsed by `settings/sources/CliSettings.java:50-56` → `applyRouterSetting` → `ReflectionUtil.setFieldValue(settings, "optimizer.enabled", "true")`, priority 60. `ReflectionUtil.copyFields` recurses into non-primitive fields (`util/ReflectionUtil.java:275-284`) instead of replacing the nested object, so the other optimizer defaults survive. **Net effect: it changes nothing over the default; it only overrides a `freerouting.json`/DSN/env source that set `false`.** The behaviourally interesting flag is `--router.optimizer.enabled=false`. |
| 9 | — | GUI hazard: `AutorouterAndRouteOptimizerThread.java:272` and `:593` unbox `optimizer.enabled` (a `Boolean`) directly, and `:273`/`:312` unbox `optimizer.maxThreads`; any settings path that leaves them null NPEs the routing thread rather than falling back. The safe accessor `getRunOptimizer()` is used only at `:604`, after the construction decision has already been made. |

#### 2.3 Fanout — including the escape-annulus contradiction

**The 2.5/4.5 vs 0.5/3.0 contradiction is resolved: both pairs are in the release and they are not
alternatives.** `2.5 / 4.5 mm` are the *configured defaults*; `0.5 / 3.0 mm` are the *hardcoded
fallbacks used only when the setting object or field is null*:

```java
// autoroute/MazeSearchAlgo.java:80-108 — anonymous TreeSet override of add()
if (ctrl.is_fanout && ctrl.fanout_start_pin_center != null) {
  boolean onStartLayer = p_element.next_room != null
      && p_element.next_room.get_layer() == ctrl.fanout_start_pin_layer;
  if (onStartLayer) {
    double maxLen = (ctrl.settings.fanout != null && ctrl.settings.fanout.maxEscapeLengthMm != null)
        ? ctrl.settings.fanout.maxEscapeLengthMm * 1000.0 : 3000.0;          // :87-88  (µm)
    double resolution = ...get_resolution(Unit.UM);
    FloatPoint entry_point = p_element.shape_entry.a.middle_point(p_element.shape_entry.b);
    if (entry_point.distance(pin_center_float) > maxLen * resolution) return false;   // :92
  }
  if (p_element.door instanceof ExpansionDrill drill) {
    double minLen = (ctrl.settings.fanout != null && ctrl.settings.fanout.minEscapeLengthMm != null)
        ? ctrl.settings.fanout.minEscapeLengthMm * 1000.0 : 500.0;           // :97-98  (µm)
    if (drill.location.to_float().distance(pin_center_float) < minLen * resolution) return false; // :101
  }
}
return super.add(p_element);
```

| # | spec claim | release | evidence |
|---|---|---|---|
| 10 | escape annulus defaults max 4.5 mm / min 2.5 mm | **=** | `settings/sources/DefaultSettings.java:117-118` |
| 11 | "fallback 3.0 mm if unset" / "fallback 0.5 mm" | **= and live** — `3000.0` and `500.0` µm literals, reached only when `fanout` or the field is null | `autoroute/MazeSearchAlgo.java:88, :98` |
| 12 | (spec compresses) both clamps apply "while on the start-pin's layer" | **Not quite.** The **max** clamp is gated on `onStartLayer`; the **min** clamp applies to **every** `ExpansionDrill` element on **any** layer. | `MazeSearchAlgo.java:85-95` vs `:96-104` |
| 13 | is the annulus reachable headless? | **Yes.** `fanout.enabled` defaults `true` (`DefaultSettings.java:113`) → `BatchAutorouter.runBatchLoop:798` → `BatchFanout.fanout_board` → `RoutingBoard.fanout` sets `is_fanout = true` (`board/RoutingBoard.java:1018`) and `fanout_start_pin_center/_layer` (`:1042-1043`). Both guard conditions are satisfied on every headless fanout attempt. It is **unreachable during normal routing** (`is_fanout` is false there) and unreachable under `algorithm=freerouting-router-v19` (no fanout stage). |
| 14 | dedicated fanout via diameters `startViaDiameterMm`/`endViaDiameterMm`, default 0.250 mm | **DEAD SETTINGS.** Declared (`settings/FanoutSettings.java:83-91`) and defaulted (`DefaultSettings.java:119-120`), but grep over the whole tree finds **zero readers**. Do not port. |
| 15 | pin sort keys `"inner_first"`, `"outer_first"`, `"distanceToClosestOnNet"`, `"surroundingsDensity"` | **key names differ.** Release accepts `"inner_first"`, `"outer_first"`, **`"distance_to_closest_on_net"`**, **`"surroundings_density"`** (snake_case). The spec's camelCase strings would silently fall through to pin-index order. Default `"outer_first"`; hardcoded fallback also `"outer_first"`. | `autoroute/BatchFanout.java:557, 565, 573, 580`; default `DefaultSettings.java:121`, fallback `BatchFanout.java:41` |
| 16 | `surroundingsDensity` = other SMD pins within 20.0 mm, descending | **=** (`20000.0 * resolution`, comment "20.0 mm in coordinate units"); counted over the net-filtered board SMD-pin list | `BatchFanout.java:541-551, 580-586` |
| 17 | `distanceToClosestOnNet` precomputed O(P²) over all board pins | **=** — `p_routing_board.get_pins()` (all pins, not just SMD) | `BatchFanout.java:526-537` |
| 18 | components sorted desc by SMD pin count, tie asc component id | **=** | `BatchFanout.java:499-509` |
| 19 | netless SMD pins filtered out | **=** | `BatchFanout.java:45-50` |
| 20 | fanout `fanout.maxPasses` default 20 (the fanout stage's own pass cap — a different knob from the autorouter's pass cap in row 70) | **=** (default `DefaultSettings.java:114`; hardcoded fallback also 20 at `BatchFanout.java:93-94`) |
| 21 | stop when `routedCount == 0`; "v19 also requires insertErrorCount == 0" | **=** — release stops on `routed_count == 0` alone | `BatchFanout.java:111-113` |
| 22 | oscillation detector `(routedCount<<32) ^ viaCount`, `stagnationPassLimit = 3` | **=**, incl. the "14 identical passes" comment | `BatchFanout.java:95, 114-130` |
| 23 | board-hash-unchanged stop | **=** | `BatchFanout.java:134-138` |
| 24 | deadline (`fanout.timeout`) and `fanout.maxItems` (default MAX_INT) | **=**, checked both between passes (`:101-108`) and between pins (`:180-184`, `:289-297`) | |
| 25 | `ripupCosts = startRipupCosts * (passNo + 1)`, start 100; `ripupAllowed=false` → `-1` | **=** | `BatchFanout.java:155, 159-162`; `DefaultSettings.java:147` |
| 26 | per-pin limit `10000 ms * (passNo + 1)` | **=** (default `DefaultSettings.java:115`, fallback 10000L at `BatchFanout.java:157-158`, applied `:185-186`) |
| 27 | skip pins whose net class has no vias unless `fallbackToBoardVias` | **=**, and the release *additionally* requires the board's first via rule to have `via_count() > 0` for the fallback to count | `BatchFanout.java:191-203` |
| 28 | `RoutingBoard.fanout` at `board/facade/RoutingBoard.java:978` | **path differs** — release has no `facade` package: `board/RoutingBoard.java:977` |
| 29 | precondition single-layer + single-net, else ALREADY_CONNECTED; empty unconnected set → NO_UNCONNECTED_NETS | **=** | `board/RoutingBoard.java:979-996` |
| 30 | destination seeded with board bounding box on bottom+top layers when fanout | **=** — `join(board.bounding_box, 0)` and `join(board.bounding_box, ctrl.layer_count - 1)` | `MazeSearchAlgo.java:1232-1238` |
| 31 | search completes at first drill whose backtrack door is also a drill | **=** | `MazeSearchAlgo.java:296-302` |
| 32 | `ctrl.removeUnconnectedVias = false` during fanout | **=** | `board/RoutingBoard.java:1044` |
| 33 | fanout-via ripup protection factor > 1 | **=**, and the constant is `FANOUT_COST_CONST = 20000`; factor `= max(1, 20000 * (halfWidth/length)²)`; applied only when **not** in a fanout pass | `MazeSearchAlgo.java:154-189` (esp. `:155, :181-185`), gate `:1404`, `:1424` |
| 34 | ≤4 targets → closest-first then fall back to whole set; >4 → whole set | **=** | `board/RoutingBoard.java:998-1015, 1055-1072` |
| 35 | after ROUTED, `optChangedArea` with 1000 ms | **=** (release passes `new int[]{pin_net_no}` as the net filter) | `board/RoutingBoard.java:1077-1081` |
| 36 | `isPinEscaped`: contacting clean Trace, or clean Via that contacts a Trace | **broader in the release**: a contacting `ConductionArea` alone counts as escaped, and a clean Via counts if it contacts a Trace **or** a ConductionArea | `core/scoring/BoardStatistics.java:508-529` |
| 37 | fanout summary log-line format | **=** verbatim | `autoroute/BatchAutorouter.java:878-879` |

#### 2.4 Route stage — item selection and one pass

| # | spec claim | release | evidence |
|---|---|---|---|
| 38 | `getAutorouteItems` semantics (storage-order scan, `Connectable && Item && !is_routable()`, `handledItems` seeding, enqueue when `connectedSet.size() < connectable_item_count(net)` and `!has_ignored_nets()`) | **=** | `BatchAutorouter.java:250-306` |
| 39 | plane-net filter skips items already connected to a `ConductionArea` | **=** | `BatchAutorouter.java:288-294` |
| 40 | reusable `ArrayList`/`TreeSet` across passes | **=** | `BatchAutorouter.java:80-81, 252-255` |
| 41 | pass skeleton, classification, `routed>0 \|\| notRouted>0` return | **=** | `BatchAutorouter.java:431-694` (`:607-631`, `:688`) |
| 42 | `maxItems` limit "requestStop when totalItemsRouted >= max" | **= plus a consequence the spec omits**: it calls `thread.requestStop()` (ALL), not `request_stop_auto_router()`, so hitting `max_items` **also skips the optimizer stage** — documented in the release comment | `BatchAutorouter.java:496-505` |
| 43 | `removeTails(removeUnconnectedVias ? NONE : FANOUT_VIA)` at pass end | **=** | `BatchAutorouter.java:657-661`; `remove_tails` impl `:1247-1252` (`remove_trace_tails(-1, opt)` + `opt_changed_area(..., 1000)`) |
| 44 | ripup escalation `start_ripup_costs * passNo`, `ripup_allowed = true` | **=**, `start_ripup_costs` default 100 *(jar-verified)* | `BatchAutorouter.java:1279-1280`; `DefaultSettings.java:60, 147` |
| 45 | via costs 50, plane via costs 5 | **=** *(jar-verified)* | `BatchAutorouter.java:1268-1273`; `DefaultSettings.java:46, 52, 145-146` |
| 46 | per-connection budget `min(100000 * 2^(passNo-1), MAX_INT)` ms | **=** | `BatchAutorouter.java:1311-1313` (also `BatchAutorouterV19.java:433`) |
| 47 | plane nets route connected → unconnected; else unconnected → connected; `CONNECTED_TO_PLANE` short-circuit | **=** | `BatchAutorouter.java:1289-1305` |
| 48 | airline via `AutorouteAirlineCalculator.calculateAirline` | **class ABSENT**; the same min-distance-pair computation is `BatchAutorouter.calc_airline` (`:1486`) / `calculateMinDistance` (`:1721`) |
| 49 | `retainAutorouteDatabase` is false in production, opt-in via `-Dfreerouting.benchmark.retain_autoroute_database` | **half wrong.** It is false — but **hardcoded**, with no system property and no setter anywhere. `grep -r "benchmark.retain"` → 0 hits. | `BatchAutorouter.java:75, 136, 1317, 1408`; same in `BatchAutorouterThread.java:79`, `BatchAutorouterV19.java:104` |
| 50 | pull-tight accuracy default 500, time limit 1000 ms | **=** | `DefaultSettings.java:97`; `BatchAutorouter.java:57, 1331-1332` |
| 51 | necked retry when FAILED/INSERT_ERROR and `neckWidthUm > 0` (default 0 = off), half-width clamp preserving compensation deltas, one retry | **=**; release also bails out early if no active layer is actually wider than the neck | `BatchAutorouter.java:1337-1351, 1377-1421`; default `DefaultSettings.java:105` |
| 52 | strict DRC: serialize before routing, id-threshold violation check, remove new items, restore, report FAILED (default false) | **=**; release also reassigns `this.board` from the deserialized snapshot | `BatchAutorouter.java:1321, 1428-1438, 1447-1468`; default `DefaultSettings.java:106` |
| 53 | per-item DRC gated behind `FRLogger.isTraceEnabled()` | **=** | `BatchAutorouter.java:543-562` |
| 54 | statistics refreshed every `PROGRESS_STATISTICS_ITEM_INTERVAL = 10` items | **ABSENT.** No such constant in the release. The only throttle is `shouldFireBoardUpdate()` = 250 ms. | `grep PROGRESS_STATISTICS_ITEM_INTERVAL` → 0 hits; `BatchAutorouter.java:241-248, 635-645` |
| 55 | GUI events at most every 250 ms | **=** | `BatchAutorouter.java:243` |
| 56 | `PerformanceProfiler.recordPass(passNo, incompleteCount, durationMs, startRipupCosts*passNo)` | **=** | `BatchAutorouter.java:682-684` |
| 57 | incomplete count from `new DesignRulesChecker(board, null).calculateAllIncompletes()` | **=** | `BatchAutorouter.java:1760-1764`, `:456-458` |
| 58 | failure logging when ≤5 items remain or failureCount ≥ 3; `failureLog.shouldSkip` only on the dead MT path | **=**; the skip threshold is `RoutingFailureLog.FAILURE_THRESHOLD = 50` (a constant the spec omits) | `BatchAutorouter.java:620-629`; `autoroute/RoutingFailureLog.java:18`; `autoroute/BatchAutorouterThread.java:346` |

#### 2.5 Batch loop stop rules — all seven constants ship

Every constant in §5.2's table is present, live, and byte-identical in the release
`autoroute/BatchAutorouter.java:52-71` *(all jar-verified via `javap -constants`)*:

| constant | release value | line |
|---|---|---|
| `STOP_AT_PASS_MINIMUM` | 8 | `:60` |
| `STOP_AT_PASS_MODULO` | 4 | `:63` |
| `STAGNATION_PASS_LIMIT` | 10 | `:66` |
| `FANOUT_RECOVERY_STAGNATION_PASSES` | 3 | `:68` |
| `STAGNATION_SCORE_THRESHOLD` | 0.5f | `:71` |
| `MAXIMUM_TRIES_ON_THE_SAME_BOARD` | 3 | `:56` |
| `BOARD_RANK_LIMIT` | 30 (`= BoardHistory.MAX_HISTORY_SIZE`) | `:54` |
| `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` | 1000 | `:57` |

Flow deltas vs §5.2:

| # | spec claim | release |
|---|---|---|
| 59 | restore check "on passes ≥ 8 with pass % 4 == 0 (or when stop requested)" | **= plus an outer guard** the spec omits: `(bh.size() >= STOP_AT_PASS_MINIMUM) \|\| stopRequested` wraps the pass test. `BatchAutorouter.java:953-955` |
| 60 | strict `>` restore test, `restoreCount <= 3`, rank > 30 → stop | **=** for the first two; the **rank check is dead code**: `getRank` returns `1..boards.size()` and `boards.size() <= MAX_HISTORY_SIZE == BOARD_RANK_LIMIT == 30`, so `rank > 30` can never be true — despite the source comment at `:52-53` claiming the equality makes it fire. `BatchAutorouter.java:961-974`; `BoardHistory.java:29, 181-198` |
| 61 | on restore: reset pass-local counter and `lastBestScore`, clear hash set | **=** | `BatchAutorouter.java:976-993` |
| 62 | v19 same-board-hash stop "exists but is commented out" | **=**, verbatim, with the stated rationale | `BatchAutorouter.java:908-929` |
| 63 | one-time fanout recovery: fanout enabled, incompletes > 0, counter ≥ 3 → `removeTails(NONE)`, rescore, reset, mark applied, clear hash set | **=** | `BatchAutorouter.java:1067-1082` |
| 64 | pass-local stagnation (counter ≥ 10 → unrouted report + stop) | **=**; the report builder is `buildUnroutedConnectionsReport()` (`:1190-1225`), **not** a class `AutorouteUnroutedReport` (absent) | `BatchAutorouter.java:1054-1098` |
| 65 | global tracker immune to restores; `currentPass - passOfBestScore >= 10` → stop | **=** | `BatchAutorouter.java:1100-1121` |
| 66 | fully-routed boards reset the counter only when score > 0.5 | **=** | `BatchAutorouter.java:1123-1130` |
| 67 | final "best board ever" restore | **=** | `BatchAutorouter.java:1141-1159` |
| 68 | `job.board = router.board` is the only publish point | **=** for the v2 router (`BatchAutorouter.java:1161`); the release has a **second** one for the GUI-only v19 router (`BatchAutorouterV19.java:235`). Tree-wide, the only other `job.board` writes are board loading (`management/BoardLoader.java:48/56`, `management/RoutingJobScheduler.java:91`, `api/v1/JobControllerV1.java:1201`) and the GUI ctor (`AutorouterAndRouteOptimizerThread.java:203`). |
| 69 | final `removeTails(NONE)` when complete without fanout vias; `bh.clear()`; FINISHED/CANCELLED/TIMED_OUT | **=** | `BatchAutorouter.java:1163-1185` |
| 70 | `router.maxPasses` default 9999 (the autorouter pass cap's upstream default; distinct from the fanout stage's `fanout.maxPasses` = 20 in row 20 and the fanout-only sentinel 0 in §1.1) | **=** *(jar-verified default in `DefaultSettings.java:95`)*; note the cap is skipped entirely when `maxPasses <= 0` (`:931`) — see §1.2 |
| 71 | — (not in spec) | **Dead statement**: `float boardScoreBefore = ...getNormalizedScore(...)` at `:943` is computed once per pass (a full `BoardStatistics` build) and never read. Cost without benefit; do not port. |
| 72 | `autoroutePassesForOptimizingItem`: ≤ `maxPassCount` passes at a fixed base ripup cost, then `removeTails(NONE)`, returns pass count (or max+1) | **=** — release `BatchAutorouter.autoroute_passes_for_optimizing_item` is `static` and constructs a fresh router with `p_remove_unconnected_vias = true` | `BatchAutorouter.java:146-167` |
| 73 | `BoardHistory` semantics (cap 30, hash dedup, evict-worst-only-if-strictly-better, `restoreBoard(maxRestores)`, `restoreBoard(<=0)` = unlimited = `restoreBestBoard()`, Java deserialization, rwlock + `synchronized`) | **=** on every point; additionally `getMaxScore()` floors at `0` (never negative, returns 0 for an empty history) | `autoroute/BoardHistory.java:29, 48-80, 115-166, 200-213` |
| 74 | — (not in spec) | `autoroute/BoardHistoryEntry.java` is a **dead top-level class** (holds a live board + `BoardStatistics`), shadowed by `BoardHistory`'s `private static class BoardHistoryEntry` (`:200`). Both ship in the jar. Ignore the top-level one. |

#### 2.6 Scoring — exactly one formula ships

| # | spec claim | release |
|---|---|---|
| 75 | `getNormalizedScore` formula and all weights (5 000 000 / 1 000 000 / 10 / 1.0 per mm / 50) | **= verbatim** *(all jar-verified)*. `calculateScore` `core/scoring/BoardStatistics.java:552-569`; `getMaximumScore` `:571-573`; `getNormalizedScore` `:575-586`; weights `settings/sources/DefaultSettings.java:27-67, 145-151` |
| 76 | plane vias not special-cased in the score | **=** — `this.vias.totalCount * scoringSettings.viaCosts` (`:566`) |
| 77 | `maxConnections = Σ max(0, endpointCount - 1)` over Pins + ConductionAreas only, at `drc/DesignRulesChecker.java:567` | **= semantics, different line**: `drc/DesignRulesChecker.java:528-537` |
| 78 | mm normalization at `BoardStatistics.java:194-203` | **= semantics, different line**: `core/scoring/BoardStatistics.java:177-186`, used at `:562-565` |
| 79 | guard returning 0 when `maximumScore <= 0` to keep stagnation counters advancing | **=**, comment included | `BoardStatistics.java:575-586` |
| 80 | §6.2 v19 scoring (incompletes × 1 000 000, violations × 1 000, `connectableItemCount(net)-1`, string-key violation dedup) | **ABSENT from the release.** `BatchAutorouterV19` has no `calculateScore`; it calls `BoardStatistics.getNormalizedScore` (`BatchAutorouterV19.java:207`). **Only the §6.1 formula ships.** Violation dedup by `min-max-layer` key does exist, but in `drc/DesignRulesChecker.getAllClearanceViolations:45-60`. |

#### 2.7 Optimizer

| # | spec claim | release | evidence |
|---|---|---|---|
| 81 | `ReadSortedRouteItems` (x, y, layer) sweep, vias-then-traces so vias win ties, skip traces contacting a non-fixed via, stateful cursor, two full O(N) scans per `next()` | **=**, character-for-character the same algorithm | `autoroute/BatchOptimizer.java:464-558` |
| 82 | `optRouteItem` steps 1–4 (self + fork contacts if `containsOnlyUnfixedTraces`, full connection ripout, user-fixed bail, snapshot, `remove_items` + `combine_traces`) | **=** | `BatchOptimizer.java:308-363`, helper `:54-61` |
| 83 | ripup `100 × 10-at-start × 0.6-for-traces` (rounded) | **=** | `BatchOptimizer.java:366-374`; defaults `DefaultSettings.java:135-136, 147` |
| 84 | inner reroute capped at `maxAutoroutePasses = 6` | **=** | `BatchOptimizer.java:377`; `DefaultSettings.java:137` |
| 85 | improvement predicate lexicographic (incompletes ≻ vias ≻ trace length; ties = not improved); `compareTo` ascending; `improvedOver = compareTo < 0` | **=** | `autoroute/ItemRouteResult.java:30-48, 54-78` |
| 86 | v19 has extra "only adopt the new length outright when incompletes/vias strictly improved, else `min`"; v2 is simpler | **= (release is the simple form)** — plain `min_cumulative_trace_length = Math.min(old, boardStatisticsAfter.traces.totalWeightedLength)` on improvement | `BatchOptimizer.java:397-399` |
| 87 | "the *weighted* trace length metric" is what `ItemRouteResult` compares | **WRONG in the release — unit mismatch.** `ItemRouteResult` is built with `trace_length_before = min_cumulative_trace_length` (a **weighted** length, seeded from `traces.totalWeightedLength` at `:225`) and `trace_length_after = boardStatisticsAfter.traces.totalLength` (the **unweighted** raw length). The length tie-break therefore compares incommensurable quantities; on typical boards `weighted >> raw`, so the "shorter trace length" branch of `improved()` fires almost unconditionally when incompletes and vias tie. Port the intent (weighted vs weighted), not the code. | `BatchOptimizer.java:225, 390-393, 397-399`; weighted-length definition `core/scoring/BoardStatistics.java:222-241` |
| 88 | "v19 uses integer division for `via_count_after / via_count_before` — a long-standing bug; **v2 casts to float (BatchOptimizer.java:345); pick the v2 semantics**" | **The bug is NOT fixed in the release.** `ItemRouteResult.java:50` still evaluates `via_count_after / via_count_before` in `int` arithmetic (both fields are `int`), quantising the via ratio to 0 or 1. The float cast the spec points at is a *different expression* in `BatchOptimizer.opt_route_pass:264-268`. Mitigating fact: `improvement_percentage()` is consumed **only** by `BatchOptimizerMultiThreaded` (`:244`, `:262`) — it is dead on the single-threaded production path. | `ItemRouteResult.java:50`; `BatchOptimizer.java:264-268`; consumers by grep |
| 89 | `optimizer.maxPasses` 100, `maxItems` MAX_INT, user stop, `optimizer.timeout` | **=** | `BatchOptimizer.java:120-136`; `DefaultSettings.java:128-129` |
| 90 | near-perfect early exit `scoreBefore * (1 + threshold) >= 1000` | **=** | `BatchOptimizer.java:142-147` |
| 91 | `withPreferredDirections = pass % 2 != 0` | **=** | `BatchOptimizer.java:153` |
| 92 | pass improvement is score-based `(after-before)/before`; `useIncreasedRipupCosts` + no gain → drop to normal costs and continue | **=** | `BatchOptimizer.java:161-170` |
| 93 | stop when `improvement < optimizationImprovementThreshold` (default 0.01) | **=** | `BatchOptimizer.java:172-177`; `DefaultSettings.java:131` |
| 94 | `maxConsecutiveFailures = 50` aborts the pass; per-item timeout/maxItems; `ProgressThrottler(1000)` | **=** | `BatchOptimizer.java:29, 232-234, 236-277`; `DefaultSettings.java:138` |
| 95 | one `useIncreasedRipupCosts` drop rule | **Two, and they coexist.** `runBatchLoop:154` **discards** `opt_route_pass`'s return value, so the v19-style rule inside `opt_route_pass` (`:281-284`: `if (use_increased_ripup_costs && route_improved == 0) { flag = false; return -1; }`) fires on its own criterion (via/length-ratio == 0) in addition to the score-based rule at `:164-170`. Either can clear the flag. | `BatchOptimizer.java:154, 164-170, 281-284` |
| 96 | optimizer has no whole-board snapshot/restore; monotonic keep/undo per item | **=** | `BatchOptimizer.java:354-357, 397-411` |

#### 2.8 Multithreading

| # | spec claim | release |
|---|---|---|
| 97 | §8.1 v19 `BatchOptRouteMT` + v19 `OptimizeRouteTask` | **ABSENT** — no `BatchOptRoute`/`BatchOptRouteMT` classes in the release. |
| 98 | §8.2 `BatchOptimizerMultiThreaded` is a direct port with the listed differences | **PRESENT and GUI-only.** All structural claims hold: eager per-item task construction (`:223-230`), `synchronized is_winning_candidate` (`:101-134`), `PriorityQueue`-based PRIORITIZED ordering (`:156-173`), unseeded `Collections.shuffle` for RANDOM (`:180`), HYBRID rotation (`:47-70, 81-91, 147-150`), `pool.shutdown()` + `awaitTermination(1, SECONDS)` polling with `shutdownNow()` on stop (`:233-254`), `clean()` nulling fields (`OptimizeRouteTask.java:64-67`). |
| 99 | task construction `job.board.deepCopy()` + `board.getItem(itemId)` | **=** — `OptimizeRouteTask.java:22-23` (`get_item`) |
| 100 | task run creates a **fresh** `BatchOptimizer` per task with `disableSnapshots = true`, so (a) `minCumulativeTraceLength == 0.0` kills the length branch of `improved()`, (b) `useIncreasedRipupCosts == false` so tasks never get the ×10 boost | **= both regressions confirmed.** `OptimizeRouteTask.java:37`; the field is only ever set in `BatchOptimizer.opt_route_pass:225` / `BatchOptimizerMultiThreaded:142,204`, none of which the fresh instance executes. `use_increased_ripup_costs` defaults `false` (`BatchOptimizer.java:32`). |
| 101 | `replaceMasterRoutingBoardWithTheWinningCandidate` sets only `this.board`; `job.board` is never reassigned, so subsequent tasks deep-copy a stale board and may get `itemToOptimize == null` → no-op | **=** | `BatchOptimizerMultiThreaded.java:136-145`; `OptimizeRouteTask.java:22, 33-35`; grep of `job.board` writes (see #68) |
| 102 | "**MT optimizer improvements are dropped from the saved result entirely**" because SES is written from `routingJob.board` | **REFUTED for the release.** `replaceMaster...` fires `fireBoardUpdatedEvent(..., this.board)` (`:140`), and the GUI's MT listener calls `boardManager.replaceRoutingBoard(event.getBoard())` (`AutorouterAndRouteOptimizerThread.java:327`); the SES is then written from `boardManager.saveAsSpecctraSessionSes(...)` (`:642`), not from `routingJob.board`. The winner **does** reach the output. What is genuinely broken is only the *intra-pass* propagation (stale clones, null items). |
| 103 | pool threads are daemons named `"batch-optimizer-worker"` | **ABSENT.** The release's thread factory is `new Thread(r)` with only an uncaught-exception handler — non-daemon, unnamed. | `BatchOptimizerMultiThreaded.java:215-220` |
| 104 | `CountDownLatch task_completion_signal` is vestigial | **=** — `prepare_task_completion_signal()` (`:93-99`) has zero callers; `countDown()` at `:132`; nothing ever awaits it. |
| 105 | MT defaults GREEDY / PRIORITIZED / `"1:1"` / `max(1, cores-1)` | **=** | `DefaultSettings.java:130, 132-134` |
| 106 | §8.3 `BoardUpdateStrategy` / `ItemSelectionStrategy` enums identical | **=** — `autoroute/BoardUpdateStrategy.java`, `autoroute/ItemSelectionStrategy.java` |
| 107 | §9: the multi-thread **route** pass has zero callers and is dead | **=** — `BatchAutorouter.autoroute_pass_multi_thread(int)` is `private` at `:315` and grep finds no call site anywhere in the tree. |
| 108 | dead design: `maxThreads` deep copies, per-thread shuffled item list with `Random(0)`, MIN_PRIORITY daemon threads named `"Router thread #<pass>.<A..Z>"`, `BoardHistory` + `restoreBestBoard()` | **=** | `BatchAutorouter.java:120, 327-349, 380, 411`; `ThreadIndexToLetter` `:1670` |
| 109 | latent bug: `join(TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP)` waits at most 1000 ms then reads the board anyway | **=** | `BatchAutorouter.java:372` |
| 110 | — (not in spec) | The dead MT path also computes a `bestThread` by score (`:398-409`) but then adopts `bh.restoreBestBoard()` (`:411`); `bestThread` is used only for the `anyProgress` return (`:415`). Two independent "best" computations. |
| 111 | upstream 2.3.0 routes strictly sequentially | **=**, for both routers. `BatchAutorouterV19` has no MT path at all and clamps `maxThreads` to 1 (`:85-88`). |
| 112 | v19's `-mt 1` default and the "MT optimization is broken" release warning | The **warning string ships verbatim** (`AutorouterAndRouteOptimizerThread.java:598-599`), keyed on `boardManager.get_num_threads()` (the *router's* `max_threads`). But the **`-mt 1` default is v19-only**: the release defaults `router.max_threads` **and** `optimizer.max_threads` to `max(1, cores-1)` (`DefaultSettings.java:102, 130`), so on any multi-core machine the GUI silently selects `BatchOptimizerMultiThreaded` and prints the "broken" warning. Headless is unaffected (always ST). |

#### 2.9 Java-isms / debug leftovers (§11 "Java-isms observed")

| # | spec claim | release |
|---|---|---|
| 113 | per-routed-item `new RatsNest(...)` (v19 hot path) | **ABSENT** — the release routers use `DesignRulesChecker` and gate it behind `FRLogger.isTraceEnabled()` (`BatchAutorouter.java:543`). `interactive/RatsNest` survives for GUI display only. |
| 114 | `board.clone()` per pass for `diff_traces` stagnation | **ABSENT** — no clone-per-pass anywhere in the release. Its replacement is `BoardHistory` serialization (`BatchAutorouter.java:944`). |
| 115 | serialize-heavy control flow (BoardHistory byte[], strict-DRC per connection) | **=** | `BoardHistory.java:208` (`board.serialize(false)`), `:153` (`BasicBoard.deserialize`); strict-DRC snapshot `BatchAutorouter.java:1321` (per connection, when enabled) |
| 116 | clone-per-task MT optimizer, all tasks constructed eagerly, contradicting the "just enough tasks" comment | **=**, comment included | `BatchOptimizerMultiThreaded.java:227-230`; `OptimizeRouteTask.java:22` |
| 117 | O(N) `findItemOnBoard` id lookup | **ABSENT** — release uses `board.get_item(item_id)` (`OptimizeRouteTask.java:23`). |
| 118 | fresh `TreeSet`/`LinkedHashMap` per routed connection | **=** | `BatchAutorouter.java:513-514` |
| 119 | hardcoded net-94 dump shipped in a release path | **=, PRESENT** | `BatchAutorouter.java:564-605` |
| 120 | `"U27-"` pin-name fanout diagnostics | **PRESENT but relocated** — not in `BatchFanout`; it is `MazeSearchAlgo.shouldTraceFanoutDiagnostics()` at `:851-855`. |
| 121 | — (not in spec) | A third debug leftover ships: `int[] focusNets = new int[] {98, 99};` in the incomplete-count path — `drc/DesignRulesChecker.java:551`. Exclude from the port along with #119/#120. |
| 122 | ThreadMXBean CPU/alloc sampling scattered through orchestration | **=** | `BatchAutorouter.java:211-239, 802-874`; `BatchOptimizer.java:63-91`; `management/RoutingJobSchedulerActionThread.java:219-262` |
| 123 | — (not in spec) | Maze-search determinism detail worth porting: `MazeSearchAlgo` seeds its `Random` with the pass's ripup cost — `random_generator.setSeed(p_ctrl.ripup_costs)` (`:77-78`, comment "Keep v1.9 deterministic randomization across passes"). This is why the release's commented-out same-hash stop (#62) argues that identical boards can still progress. |

### 3. Features this spec attributes to "v2" — PRESENT / ABSENT in the release (porting go/no-go)

| v2 feature per the spec | release | note |
|---|---|---|
| `autoroute/pipeline/` package, `RoutingPipeline`, `AutorouteBatchLoop`, `AutoroutePassRunner`, `AutorouteConnectionRouter`, `AutorouteAirlineCalculator`, `AutorouteRuntimeMetrics`, `AutorouteUnroutedReport`, `BatchAutorouterThread` (pipeline one), `GuiRoutingJobWorker` | **ABSENT** | master-only; all folded into `autoroute/BatchAutorouter.java` |
| `BatchOptimizer.createForGui` / `createForHeadless` factories | **ABSENT** | inline construction |
| `BoardHistory` score-ranked snapshot store (cap 30) | **PRESENT** | `autoroute/BoardHistory.java` |
| `STOP_AT_PASS_MINIMUM` / `_MODULO` / `STAGNATION_*` / `MAXIMUM_TRIES_ON_THE_SAME_BOARD` / `BOARD_RANK_LIMIT` / `FANOUT_RECOVERY_STAGNATION_PASSES` | **PRESENT** (all 7, jar-verified) | `BatchAutorouter.java:52-71`; `BOARD_RANK_LIMIT` check is unreachable (#60) |
| board-restore machinery (`restoreBoard(3)`, rank cap, final best-board restore) | **PRESENT** | `BatchAutorouter.java:953-996, 1141-1159` |
| pass-local **and** global stagnation trackers | **PRESENT** | `:1054-1121` |
| one-time fanout-tail recovery | **PRESENT** | `:1067-1082` |
| v2 scoring (`BoardStatistics.getNormalizedScore`, 5 M / 1 M / 10 / 1-per-mm / 50, pins+pours `maxConnections`, mm normalization) | **PRESENT** | the **only** scoring path in the release |
| plane-net item filter in `getAutorouteItems` | **PRESENT** | `:288-294` |
| `RoutingFailureLog` (+ `FAILURE_THRESHOLD = 50`) | **PRESENT** | used for logging on the live path, for skipping only on the dead MT path |
| strict-DRC snapshot/rollback (`strict_drc`, default false) | **PRESENT** | `:1321, 1428-1468` |
| necked retry (`neck_width_um`, default 0) | **PRESENT** | `:1377-1421` |
| fanout: netless-pin filter, configurable pin sort order, oscillation detector, `max_items`, `timeout`, `ripup_allowed`, `fallback_to_board_vias` | **PRESENT** | `autoroute/BatchFanout.java`; sort-order **key spellings differ** (#15) |
| fanout escape annulus (`min/max_escape_length_mm`) | **PRESENT and reachable headless** | `MazeSearchAlgo.java:80-108` |
| fanout dedicated via diameters (`start/end_via_diameter_mm`) | **PRESENT as settings, ABSENT as behaviour** | zero readers — do not port |
| `BatchOptimizerMultiThreaded` + `OptimizeRouteTask` | **PRESENT, GUI-only, semantically broken** | unreachable headless; see #100–#104 |
| multi-threaded **route** pass (`autoroute_pass_multi_thread` + `autoroute/BatchAutorouterThread`) | **PRESENT as dead code** | zero callers (#107) |
| `ProgressThrottler(1000)` / 250 ms `shouldFireBoardUpdate` | **PRESENT** | `BatchFanout.java:29`, `BatchOptimizer.java:29`, `BatchAutorouter.java:241-248` |
| `PROGRESS_STATISTICS_ITEM_INTERVAL = 10` | **ABSENT** | master-only |
| `-Dfreerouting.benchmark.retain_autoroute_database` opt-in | **ABSENT** | hardcoded `false` |
| `SettingsMerger` 8-source priority chain, `DefaultSettings` as the single source of scoring truth | **PRESENT** | `settings/SettingsMerger.java:33-48`, `settings/sources/DefaultSettings.java` |
| `ItemRouteResult` float via-ratio fix | **ABSENT** | integer division still ships (#88) |

### 4. Port-from verdict — the authoritative release file per algorithm

Everything below is under `/tmp/fr-java-release/src/main/java/app/freerouting/`.

| algorithm / subsystem | port from | notes |
|---|---|---|
| Batch loop, pass scheduling, all stop rules, board-restore, stagnation, final best-board | `autoroute/BatchAutorouter.java:757-1188` (`runBatchLoop`) | the single authority; constants at `:52-71` |
| Route pass body (item loop, classification, tail removal, accounting) | `autoroute/BatchAutorouter.java:431-694` (`autoroute_pass`) | |
| Route work-list selection | `autoroute/BatchAutorouter.java:250-306` (`getAutorouteItems`) | |
| Per-connection routing (costs, budgets, direction, engine init) | `autoroute/BatchAutorouter.java:1256-1366` (`autoroute_item`) | + `retryConnectionNecked:1377`, `applyStrictDrcAfterRoute:1428`, `enforceStrictDrc:1447` |
| Optimizer inner reroute driver | `autoroute/BatchAutorouter.java:146-167` (`autoroute_passes_for_optimizing_item`) | |
| Fanout stage orchestration, pass loop, pin/component ordering | `autoroute/BatchFanout.java` (loop `:82-144`, pass `:147-~360`, ordering `:449-594`) | |
| Fanout escape mechanics (implicit via placement, annulus, drill-drill completion, destination seeding, fanout-via ripup protection) | `autoroute/MazeSearchAlgo.java:80-108` (annulus), `:296-302` (completion), `:1232-1238` (destination), `:154-189` + `:1395-1424` (protection) | |
| Per-pin fanout attempt (preconditions, target ordering, via-rule fallback, pull-tight) | `board/RoutingBoard.java:977-1083` (`fanout`) | no `facade` package in the release |
| Board score | `core/scoring/BoardStatistics.java:552-586` (+ `:177-186` mm normalization, `:222-241` weighted length, `:508-529` `isPinEscaped`) and `drc/DesignRulesChecker.java:528-537` (`max_connections`) | |
| Scoring / router / fanout / optimizer default constants | `settings/sources/DefaultSettings.java` | single source of truth; jar-verified |
| Optimizer batch loop and stop rules | `autoroute/BatchOptimizer.java:96-207` (`runBatchLoop`) + `:215-296` (`opt_route_pass`) | note the two independent ripup-drop rules (#95) |
| Optimizer per-item ripout/reroute/keep-or-undo | `autoroute/BatchOptimizer.java:308-414` (`opt_route_item`) | fix the weighted/unweighted mismatch (#87) when porting |
| Optimizer sweep order | `autoroute/BatchOptimizer.java:464-558` (`ReadSortedRouteItems`) | replace the O(N²) scan with an ordered index, per §11.2 |
| Improvement predicate / result record | `autoroute/ItemRouteResult.java:30-78` | port `improved()`/`compareTo` as-is; **do not** port `improvement_percentage` (`:50`, integer division, MT-only) |
| Board snapshot history | `autoroute/BoardHistory.java` | ignore the shadowed dead `autoroute/BoardHistoryEntry.java` |
| Failure bookkeeping | `autoroute/RoutingFailureLog.java` (`FAILURE_THRESHOLD = 50`) | |
| Settings resolution / precedence | `settings/SettingsMerger.java` + `settings/sources/*` + `util/ReflectionUtil.java:145-290` | note the fail-closed `getRunOptimizer`/`isFanoutEnabled` vs fail-open `getRunRouter`/`getRunFanout` |
| Stage sequencing (headless) | `management/RoutingJobSchedulerActionThread.java:38-217` | the golden reference for a headless Rust driver |
| Stage sequencing (GUI, for reference only) | `interactive/AutorouterAndRouteOptimizerThread.java:199-330, 451-701` | |
| **Do not port** | `autoroute/BatchAutorouterV19.java` (GUI-only comparison shim, no fanout, no stop machinery), `autoroute/BatchOptimizerMultiThreaded.java` + `autoroute/OptimizeRouteTask.java` (GUI-only, stale-clone semantics), `autoroute/BatchAutorouterThread.java` + `BatchAutorouter.autoroute_pass_multi_thread` (dead), `autoroute/BoardHistoryEntry.java` (dead), fanout via-diameter settings (inert), net-94 / `U27-` / `focusNets{98,99}` debug dumps | |

**Net effect on §11 (Rust design implications).** Items 1–6 of "Keep semantically" all survive the
release audit unchanged, with three amendments: (a) §11.1's "maxPasses=0 trick replaced by an
explicit `fanout_only` mode" is now *required*, not cosmetic (§1.2); (b) §11.5's "pick the v2
semantics" for the integer-division bug is a **deliberate divergence from the release**, not a port
(#88); (c) §11.6's escape annulus "2.5–4.5 mm" should be encoded as configurable defaults with the
release's null-fallbacks (0.5/3.0 mm) documented but not replicated as magic behaviour (#10–#12).
