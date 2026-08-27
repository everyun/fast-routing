# Scope Map, Capability Surface (RouterSettings), Trace Insert/Split/Merge, and the v1.9 → v2 Delta

> **Role of this document.** The five sibling specs in this directory each cover one *algorithm*.
> This one covers **everything else that a working router still needs**, plus the
> **architecture-level diff between Freerouting v1.9 and v2.x (2.3.0)**. It is deliberately
> breadth-first with depth on the parts nobody else documented: the settings/capability surface,
> the board mutation & normalization contract, the undo model, the rules model, the DRC/incompletes
> model, and the headless pipeline plumbing.
>
> Sibling specs (read them for the algorithms; this doc cross-references, it does not repeat):
> * `maze-engine.md` — expansion rooms, doors, A*, drills, ripup, `DestinationDistance`.
> * `shove.md` — `ShapeTraceEntries`, trace/via/pad shoving, spring-over, maze-time shove.
> * `search-tree-obstacles.md` — `ShapeSearchTree`/`MinAreaTree`, clearance compensation, tree shapes.
> * `pull-tight-insert.md` — connection locate/insert, pull-tight, and the *intra-trace*
>   `split()`/`combine()`/`normalize()` algorithms (v19 line numbers).
> * `batch-orchestration.md` — `RoutingPipeline`, fanout/route/optimize pass scheduling, scoring formula, MT optimizer.
>
> Source trees studied:
> * v2.x: `/tmp/fr-java/src/main/java/app/freerouting` — 561 `.java` files.
> * v1.9: `/tmp/fr-java/src_v19/main/java/app/freerouting` — 349 `.java` files.
>
> **All line references in this file were read directly from source.** Where a fact came from v19
> the path is prefixed `v19:`.

---

## Table of contents

1. [Package maps and the coverage matrix](#1-package-maps-and-the-coverage-matrix)
2. [v1.9 → v2: what actually changed](#2-v19--v2-what-actually-changed)
3. [(a) The RouterSettings capability checklist — parity scoreboard](#3-a-the-routersettings-capability-checklist--parity-scoreboard)
4. [The settings pipeline: sources, priorities, merge semantics](#4-the-settings-pipeline-sources-priorities-merge-semantics)
5. [Board-geometry-derived cost initialization (`applyBoardSpecificOptimizations`)](#5-board-geometry-derived-cost-initialization-applyboardspecificoptimizations)
6. [The headless pipeline, end to end](#6-the-headless-pipeline-end-to-end)
7. [core/* — job model, stop protocol, counters, manifest](#7-core--job-model-stop-protocol-counters-manifest)
8. [Scoring: `BoardStatistics` field-by-field](#8-scoring-boardstatistics-field-by-field)
9. [The board item model: `Item`, fixed states, the obstacle matrix](#9-the-board-item-model-item-fixed-states-the-obstacle-matrix)
10. [Connectivity primitives: contacts, connected sets, connections, cycles, tails](#10-connectivity-primitives-contacts-connected-sets-connections-cycles-tails)
11. [(b) Trace insert / split / merge — the board-level contract](#11-b-trace-insert--split--merge--the-board-level-contract)
12. [Areas: ObstacleArea, ViaObstacleArea, ComponentObstacleArea, ConductionArea, BoardOutline](#12-areas-obstaclearea-viaobstaclearea-componentobstaclearea-conductionarea-boardoutline)
13. [Undo/redo: `UndoableObjects`](#13-undoredo-undoableobjects)
14. [The rules model: BoardRules / ClearanceMatrix / NetClass / ViaRule](#14-the-rules-model-boardrules--clearancematrix--netclass--viarule)
15. [DRC and incompletes (ratsnest)](#15-drc-and-incompletes-ratsnest)
16. [The numeric substrate: geometry, `Limits`, and the BigInteger hot path](#16-the-numeric-substrate-geometry-limits-and-the-biginteger-hot-path)
17. [I/O scope: DSN/SES/KiCad-JSON/rules](#17-io-scope-dsnseskicad-jsonrules)
18. [(c) Things nobody else's spec covers that a rewrite must not forget](#18-c-things-nobody-elses-spec-covers-that-a-rewrite-must-not-forget)
19. [Rust design implications](#19-rust-design-implications)

---

## 1. Package maps and the coverage matrix

### 1.1 v2.x package layout (561 files)

```
app.freerouting
├── Freerouting.java                (1497 L)  process entry point, CLI/API/MCP/GUI bootstrap
├── analytics/ + analytics/dto      telemetry (PostHog-style); NOT needed for a rewrite
├── api/  (dev, dto, mcp, security, v1)       Jetty+Jersey REST API + MCP server
├── autoroute/                      SEE maze-engine.md, batch-orchestration.md, pull-tight-insert.md
│   ├── drill/  expansion/  maze/  path/  pipeline/  events/
├── board/
│   ├── actions/    DrillItemMover, ForcedPadRouter, ForcedViaInserter (→ shove.md),
│   │               ItemIdGenerator, ItemInfoPrinter, ItemSearchTreesInfo, ItemSelectionFilter,
│   │               MoveComponent
│   ├── facade/     BasicBoard (1487), RoutingBoard (1442), BoardItemRepository,
│   │               BoardConnectivityQueries, BoardSnapshotManager, RoutingBoardOperations,
│   │               RoutingBoardSearchFacade, RoutingBoardUndoFacade        ← §11, §13 HERE
│   ├── model/items/    Item (1299), Trace, Pin (706), Via, DrillItem, ConductionArea,
│   │                   ObstacleArea, ViaObstacleArea, ComponentObstacleArea, ComponentOutline,
│   │                   Connectable, BoardItemType, PrintableShape                ← §9, §12 HERE
│   ├── model/structure/ AngleRestriction, BoardOutline, Component(s), FixedState, Layer,
│   │                    LayerStructure, ShapeAndEntrySide, ShapeEntrySide, Unit
│   ├── optimize/   TraceShover, TraceTightener{,45,90,AnyAngle}, ViaOptimizer  → shove/pull-tight
│   ├── searchtree/ → search-tree-obstacles.md
│   ├── state/      BoardComparator, BoardObserverAdaptor/Observers, ChangedArea, Communication,
│   │               CoordinateTransform
│   └── trace/      PolylineTrace (1314), PolylineTraceGeometry,
│                   PolylineTraceNormalization, PolylineTraceSearchTreeAdapter   ← §11 HERE
├── core/           RoutingJob (548), StoppableThread, RoutingJobState/Stage/Priority,
│                   BoardFileDetails, RouterCounters, RouterJobResourceUsage, Session,
│                   ProgressThrottler, events/, library/, results/, scoring/     ← §7, §8 HERE
├── datastructures/ UndoableObjects (343), PlanarDelaunayTriangulation (978), ShapeTree,
│                   MinAreaTree, BigIntAux, TimeLimit, Stoppable, IdGenerator,
│                   ArrayStack, Signum, IndentFileWriter, Observers               ← §13, §15, §16
├── debug/          DebugControl / trace-event plumbing (see §16 Java-isms)
├── drc/            DesignRulesChecker (821), NetIncompletes (398), AirLine,
│                   ClearanceViolation, UnconnectedItems                          ← §15 HERE
├── geometry/planar/ 11 832 L of exact/approx 2-D geometry                        ← §16 HERE
├── gui/            Swing; a11y, board, controls, interactive, menus, rendering, support,
│                   windows, workspace                     (gui/workspace/progress/RatsNest is
│                                                           imported by headless BoardStatistics)
├── io/             specctra/ (+parser/), kicad/, CoordinateTransform, BoardReadResult  ← §17
├── logger/         FRLogger + log4j2 config factory
├── management/     BoardLoader, BoardManager, HeadlessBoardManager (1011),
│                   jobs/RoutingJobScheduler (578) + ActionThread (296),
│                   sessions/SessionManager                                        ← §6 HERE
├── rules/          BoardRules, ClearanceMatrix, NetClass(es), Net(s), ViaInfo(s), ViaRule,
│                   DefaultItemClearanceClasses                                    ← §14 HERE
├── settings/       RouterSettings (966) + 17 sub-setting classes + sources/       ← §3, §4 HERE
└── util/           ReflectionUtil, TextManager, GsonProvider, SafeObjectInputStream
```

### 1.2 v1.9 package layout (349 files) and the rename map

| v1.9 | v2.x | Nature |
|---|---|---|
| `board/*` (flat, 55 files) | `board/{facade,model/items,model/structure,actions,searchtree,optimize,state,trace}` | pure move + camelCase rename |
| `board/CalcFromSide` | `board/model/structure/ShapeEntrySide` | rename (+1 semantic delta, see shove.md §13.1) |
| `board/CalcShapeAndFromSide` | `board/model/structure/ShapeAndEntrySide` | rename |
| `board/ForcedPadAlgo` / `ForcedViaAlgo` | `board/actions/ForcedPadRouter` / `ForcedViaInserter` | rename (+ delta, shove.md §13.2) |
| `board/MoveDrillItemAlgo` | `board/actions/DrillItemMover` | rename |
| `board/ShoveTraceAlgo` | `board/optimize/TraceShover` | rename |
| `board/PullTightAlgo*` | `board/optimize/TraceTightener*` | rename |
| `board/OptViaAlgo` | `board/optimize/ViaOptimizer` | rename |
| `board/ObjectInfoPanel` | `board/actions/ItemInfoPrinter` | GUI decoupling |
| `board/ItemIdNoGenerator` | `board/actions/ItemIdGenerator` | rename |
| `board/ClearanceViolation` | `drc/ClearanceViolation` | move |
| `board/TestLevel` | **deleted** | v2 uses `settings/DebugSettings` + `debug/DebugControl` |
| `autoroute/MazeSearchAlgo` (1729 L) | `autoroute/maze/MazeSearchEngine` (1258) + `MazeExpansionEngine` (415) + `MazeRipupResolver` (269) + `MazeFanoutDiagnostics` (44) | split, ≈ same total |
| `autoroute/MazeShoveTraceAlgo` | `autoroute/maze/MazeTraceShover` | rename |
| `autoroute/LocateFoundConnectionAlgo*` | `autoroute/path/FoundConnectionLocator*` | rename |
| `autoroute/InsertFoundConnectionAlgo` (524) | `autoroute/path/FoundConnectionInserter` (807) | rename + **grew** (neckdown, strict-DRC hooks) |
| `autoroute/BatchAutorouter` (789) | `autoroute/pipeline/{BatchAutorouter 565, AutorouteBatchLoop 609, AutoroutePassRunner 526, AutorouteConnectionRouter 255, BatchAutorouterThread 621, AutorouteAirlineCalculator 214, …}` | **large split + new features** |
| `autoroute/BatchOptRoute` / `BatchOptRouteMT` | `autoroute/pipeline/BatchOptimizer` / `BatchOptimizerMultiThreaded` | rename |
| `autoroute/BatchFanout` (312) | `autoroute/pipeline/BatchFanout` (780) | **rewritten/extended** |
| `interactive/AutorouteSettings` + `interactive/Settings` | `settings/RouterSettings` (+ `Scoring/Optimizer/Fanout/LayerSettings`) | **restructured, JSON-serializable, nullable-merge** |
| `interactive/RatsNest` / `NetIncompletes` | `drc/DesignRulesChecker` / `drc/NetIncompletes` + `gui/workspace/progress/RatsNest` | move + **semantic delta**, §15 |
| `interactive/BoardHandling{,Headless}` | `management/{BoardManager,HeadlessBoardManager}` + `gui/workspace/GuiBoardManager` | restructure |
| `interactive/InteractiveActionThread` | `core/StoppableThread` + `management/jobs/RoutingJobSchedulerActionThread` | restructure |
| `designforms/specctra/*` | `io/specctra/` + `io/specctra/parser/` | move |
| `boardgraphics/*` | `gui/rendering/` | move |
| `library/*` | `core/library/` | move |
| `tests/*` | **deleted** (moved to a real test source root) | — |
| — | `api/`, `analytics/`, `drc/`, `io/kicad/`, `settings/sources/`, `management/jobs/`, `core/scoring/`, `core/results/`, `autoroute/events/`, `autoroute/BoardHistory` | **new in v2** |

### 1.3 Coverage matrix — who documents what

| Area | Covered by |
|---|---|
| Maze search, rooms/doors, drills, ripup, A* heuristic | `maze-engine.md` |
| Shoving (trace/via/pad), spring-over, `ShapeTraceEntries` | `shove.md` |
| Search tree, clearance compensation, tree shapes, `complete_shape` | `search-tree-obstacles.md` |
| Locate/insert connection, pull-tight, intra-trace split/combine/normalize algorithms | `pull-tight-insert.md` |
| Pipeline stages, pass scheduling, MT optimizer, score formula | `batch-orchestration.md` |
| **RouterSettings surface + defaults + merge** | **this doc §3–§5** |
| **Headless bootstrap, job scheduler, board load, output writing** | **this doc §6–§7** |
| **Item model, fixed states, obstacle predicate matrix** | **this doc §9** |
| **Contacts / connected sets / connections / cycles / tails** | **this doc §10** |
| **Board-level trace insert & the split-at-junction contract** | **this doc §11** |
| **Area item semantics (keepouts, planes, outline)** | **this doc §12** |
| **Undo/redo model** | **this doc §13** |
| **Rules: clearance matrix, net classes, via rules** | **this doc §14** |
| **Incompletes/ratsnest (Delaunay+Kruskal), DRC** | **this doc §15** |
| **Geometry & exact arithmetic substrate** | **this doc §16 (scope only — needs its own spec)** |
| **DSN/SES/KiCad I/O** | **this doc §17 (scope only — needs its own spec)** |
| GUI, REST API, MCP, analytics | *intentionally out of scope for the rewrite* |

---

## 2. v1.9 → v2: what actually changed

**Verdict: v2's router core is ~85% reorganized-and-instrumented v19, with a small set of real
behavioral additions and two real bug-fix classes.** The maze search, shove, pull-tight, search-tree
and room-completion algorithms are line-for-line recognizable; the file split, `camelCase`
rename, and the addition of `FRLogger.trace(...)` instrumentation account for most of the diff.

### 2.1 Real behavioral additions in v2 (not in v19)

| # | Feature | Evidence | Impact |
|---|---|---|---|
| 1 | **Per-layer bend cost in maze expansion.** `ctrl.bendCosts[layer]` added to `g` when the path turns; trigger is `cross² > 0.01·|prev|²·|next|²` (≈5.7°). | `maze/AutorouteControl.java:25,144-147`, `maze/MazeSearchEngine.java:855-878` | v19 has *no* bend term at all in the maze. Default `defaultBendCost = 0.0` ⇒ off unless configured. |
| 2 | **Pure-SMD-net via relaxation.** If every connectable item of the net is a single-layer pin: `attachSmdAllowed` forced `true` even if all padstacks say no; and `viaCostFactor *= 0.1`. | `maze/AutorouteControl.java:189-202, 263-281` | v19 `init_net` has neither (`v19:AutorouteControl.java:130-189`). Materially changes fanout on SMD-only boards. |
| 3 | **Power-plane layers forced non-routable.** If `!layerStructure.layers[i].isSignal` but the setting says active ⇒ warn + force `layerActive[i]=false`. | `maze/AutorouteControl.java:151-162` | v19 just copied `layer_active_arr`. |
| 4 | **`strict_drc`** — serialize board, route, re-check clearance violations on *new* items of the net, and on violation remove the new items **and roll the whole board back from the snapshot**. | `pipeline/AutorouteConnectionRouter.java:84-85, 242-255`; `pipeline/BatchAutorouter.java:305-329` | new capability; default off. |
| 5 | **`neck_width_um`** — one retry of a failed connection with all trace half-widths clamped. | `pipeline/AutorouteConnectionRouter.java:125,177,238` | new; default 0 (off). |
| 6 | **`copper_to_edge_clearance_um` / `hole_clearance_um` overrides** — synthesize `board_edge` / `hole_edge` clearance classes and rewrite the clearance matrix after board load. | `management/HeadlessBoardManager.java:346-552` | new; defaults 500 µm / 0 µm. |
| 7 | **Escape vias** (`insertEscapeVia`, `Via.isEscapeVia` + `escapeViaSmdLayer`) with SMD-layer clearance exception. | `board/facade/BasicBoard.java:310-330` | new; feeds the rewritten `BatchFanout`. |
| 8 | **Fanout enabled by default** (`fanout.enabled = true`, `max_passes=20`, `max_ms_per_pin=10000`, escape length 2.5–4.5 mm, via Ø 0.25 mm, `pin_sorting_order="outer_first"`). | `settings/sources/DefaultSettings.java:117-127` | v19 default was `with_fanout = false` (`v19:AutorouteSettings.java:45`). **Biggest single behavioral difference out of the box.** |
| 9 | **`BoardHistory`** (v2-only) — best-board memory across passes; see `batch-orchestration.md` §5.3. | `autoroute/BoardHistory.java` | new |
| 10 | **Incompletes filtering** — dangling (`isTail()`) items and zero-contact non-drill/non-plane items are excluded before the Delaunay/Kruskal ratsnest. | `drc/NetIncompletes.java:76-115` | v19 had no filter (`v19:NetIncompletes.java:33-45`). **Changes the reported incomplete count and therefore the score.** |
| 11 | **Trace-length normalized to mm** in `calculateScore` (`totalLengthMm`), to stop resolution-dependent score collapse. | `core/scoring/BoardStatistics.java:190-203, 603-613` | v19 used raw board units. |
| 12 | **`normalize` convergence guards** — `MAX_NORMALIZE_ITERATIONS = 2000` outer cap + per-net suppression set; `MAX_NORMALIZATION_DEPTH = 16` now *logs & returns false* instead of throwing. | `board/facade/BasicBoard.java:64, 709-795`; `board/trace/PolylineTraceNormalization.java:16,26-41` | v19 `normalize_traces` was `throws Exception` and could spin. |
| 13 | **Iterative `combine()`** instead of recursive (StackOverflow fix on long collinear chains). | `board/trace/PolylineTrace.java:175-192` vs `v19:PolylineTrace.java:185-205` | bug fix |
| 14 | **Deletion guards in split/combine** — `split(lineIndex,line)` returns null if `isDeletionForbidden()`; `combineAtStart/End` require both traces deletable; degenerate USER_FIXED traces are skipped instead of infinitely retried. | `PolylineTrace.java:727-729, 246-247`; `PolylineTraceNormalization.java:102-121` | bug fix (infinite loops) |
| 15 | **Search-tree-entry fallback in combine** — if either trace lacks default-tree entries, fall back to remove+reinsert instead of NPE-ing in `mergeEntriesInFront`. | `PolylineTrace.java:304-323` | bug fix |
| 16 | **REST API + MCP server + job scheduler + sessions**, KiCad JSON I/O, result manifest JSON. | `api/`, `management/jobs/`, `io/kicad/`, `core/results/` | new |
| 17 | Timeouts per stage (`fanout.timeout`, `optimizer.timeout`, `job_timeout`), resource-usage sampling, per-stage analytics. | `settings/FanoutSettings.java:105`, `OptimizerSettings.java:94`, `management/jobs/RoutingJobSchedulerActionThread.java:44-90, 208-257` | new |

### 2.2 What did **not** change semantically

* Cost constants that survived verbatim: `via_costs = 50`, `plane_via_costs = 5`,
  `start_ripup_costs = 100`, `pull_tight_accuracy = 500`, `max_shove_trace_recursion_depth = 20`,
  `max_shove_via_recursion_depth = 5`, `max_spring_over_recursion_depth = 5`,
  `ripup_costs = 1000` / `ripup_pass_no = 1` initial, `clearance_safety_margin = 16`,
  `Limits.CRIT_INT = 2^25`, `MAX_NORMALIZATION_DEPTH = 16`, default trace half width 1500,
  `defaultClearanceClass() = 1`, `clearanceClassNone() = 0`.
* The board-aspect-ratio cost initialization (§5) is byte-identical arithmetic to
  `v19:AutorouteSettings.java:51-87`.
* `min_normal_via_cost = viaCosts · max(maxViaRadius, 1)`, `min_cheap_via_cost = 0.8 ·
  min_normal_via_cost` — identical (modulo the SMD ×0.1 in v2).
* Shove, pull-tight, room completion, drill pages: see sibling specs; deltas there are small and
  already documented.

### 2.3 Which one does the 2.3.0 release actually run?

`RouterSettings.ALGORITHM_CURRENT = "freerouting-router"` is the default
(`DefaultSettings.java:97`); `ALGORITHM_V19 = "freerouting-router-v19"` exists only as a legacy
compatibility token (`RouterSettings.java:15-16`). Everything under `autoroute/pipeline/` is the
live path; nothing from `src_v19` ships in 2.3.0. **When v2 and v19 disagree, v2 wins for parity
purposes** — but v19 remains the clearer reference for the *algorithms*, because v2 files are
heavily interleaved with `FRLogger.trace` debug scaffolding.

---

## 3. (a) The RouterSettings capability checklist — parity scoreboard

Everything below is a knob that a caller can set (JSON, env, CLI, API, DSN, rules file).
"Default" is what `settings/sources/DefaultSettings.java` produces. "Consumer" is the code that
actually reads it — **note the entries marked *dead in headless*.**

### 3.1 Top-level `router.*` (`settings/RouterSettings.java`)

| JSON key | Java field | Type | Default | Semantics / consumer |
|---|---|---|---|---|
| `enabled` | `enabled` | Bool | `true` | Run the autoroute stage at all. `getRunRouter()` defaults to `true` when null. Read by `RoutingJobSchedulerActionThread:92-95` and `RoutingPipeline`. |
| `algorithm` | `algorithm` | String | `"freerouting-router"` | Selector token; `"freerouting-router-v19"` reserved. |
| `job_timeout` | `jobTimeoutString` | String `"HH:MM:SS"` / `"5m"` | `"12:00:00"` | Parsed by `TextManager.parseTimespanString`; **clamped to 24 h** (`MAX_TIMEOUT`), grace period 30 s after `requestStop()` before state → `TIMED_OUT` (`RoutingJobSchedulerActionThread.java:24-25, 44-85`). |
| `max_passes` | `maxPasses` | Int | `9999` | Ripup-and-reroute pass cap. `validate()`: `<0 or >9999 ⇒ 9999`; **`0` ⇒ `Integer.MAX_VALUE` (no limit)** (`RouterSettings.java:932-941`). CLI `-mp` clamps to `[0,9999]` before merge. |
| `max_items` | `maxItems` (transient) | Int | `Integer.MAX_VALUE` | Hard cap on `routedCount + failedCount`; stops the auto-router (`pipeline/BatchAutorouterThread.java:346-350`). |
| `max_threads` | `maxThreads` | Int | `max(1, cores-1)` | `normalizeMaxThreads`: `null`/`<0` ⇒ `cores-1`; `0` ⇒ `cores`; else `min(v, cores)`. Setter mirrors into `optimizer.maxThreads` (`RouterSettings.java:133-149, 176-186`). |
| `trace_pull_tight_accuracy` (alt `tracePullTightAccuracy`) | `tracePullTightAccuracy` | Int | `500` | Pull-tight `min_translate_dist` seed; `validate()` forces `>=1` else 500. → `PullTightAlgo` / `TraceTightener` (see pull-tight-insert.md §5.1). |
| `allowed_via_types` | `viasAllowed` | Bool | `true` | Whether the maze may change layers at all → `AutorouteControl.viasAllowed`. (Key name is a historical misnomer; it is a boolean.) |
| `automatic_neckdown` (alt `automaticNeckdown`) | `automaticNeckdown` | Bool | `true` | → `AutorouteControl.withNeckdown` (`AutorouteControl.java:168`), consumed at `path/FoundConnectionInserter.java:146,205` and `maze/MazeSearchEngine.java:407,442`. |
| `save_intermediate_stages` | `saveIntermediateStages` (transient) | Bool | `false` | Dump per-pass boards (`pipeline/AutorouteBatchLoop.java:408`). |
| `ignore_net_classes` | `ignoreNetClasses` (transient) | String[] | `[]` | CLI `-inc a,b`. **Dead in headless mode.** Only `gui/board/GuiManager.java:466-473` translates it into `NetClass.isIgnoredByAutorouter`, which is what `Item.hasIgnoredNets()` → `pipeline/BatchAutorouter.java:375` reads. A CLI run never applies it. |
| `copper_to_edge_clearance_um` | `copperToEdgeClearanceUm` | Double | `500.0` | Post-load: creates/reuses a `board_edge` clearance class, sets it symmetrically against **every** class on **every** layer, reassigns the `BoardOutline` to it, and re-inserts the outline into the search tree. **Skipped** when the value equals the default *and* the DSN gave the outline a non-default class (`HeadlessBoardManager.java:466-552`). |
| `hole_clearance_um` | `holeClearanceUm` | Double | `0.0` | Sets `BoardRules.holeClearance`; additionally reclassifies every **circular, component-owned `ObstacleArea`** (KiCad NPTH representation) into a synthesized `hole_edge` class whose matrix values are `max(holeClearance, defaultAreaClearance)`; forces `searchTreeManager.reinsertTreeItems()` when anything changed (`HeadlessBoardManager.java:346-464`). |
| `neck_width_um` | `neckWidthUm` | Double | `0.0` | >0 ⇒ one retry of a failed connection with all half-widths clamped to this width (`AutorouteConnectionRouter.java:125-238`). |
| `strict_drc` | `strictDrc` | Bool | `false` | If a routed connection's *new* traces/vias carry any clearance violation, remove them and restore the pre-route board snapshot; count as not-routed. Costs one full `board.serialize(false)` per connection attempt. |
| `result_json` | `resultJsonPath` | String | `null` | Write `RoutingResultManifest` JSON (schema v1) at CLI exit. |
| `layers` | `layers[]` (transient) | `LayerSettings[]` | `null` (sized from DSN) | See §3.2. |
| `fanout` | `fanout` | object | see §3.3 | |
| `optimizer` | `optimizer` | object | see §3.4 | |
| `scoring` | `scoring` | object | see §3.5 | |

Constants: `MIN_BEND_COST = 0.0`, `MAX_BEND_COST = 9.9` (`RouterSettings.java:17-18`).

### 3.2 `router.layers[i]` (`settings/LayerSettings.java`)

| Key | Type | Default | Semantics |
|---|---|---|---|
| `routable` | Bool | `true` for signal layers, forced `false` for non-signal | `getLayerActive(i)`; null ⇒ `true`. Non-signal layers are additionally force-disabled inside `AutorouteControl` with a warning. |
| `preferred_direction_horizontal` | Bool | alternating, seeded from board aspect ratio (§5) | null-fallback in `getPreferredDirectionIsHorizontal` is `layer % 2 == 1`. |
| `bend_cost` | Double | `scoring.default_bend_cost` (`0.0`) | Clamped to `[0.0, 9.9]` on set and on default-read. Feeds `AutorouteControl.bendCosts[i]`. **v2 only.** |

### 3.3 `router.fanout.*` (`settings/FanoutSettings.java`)

| Key | Type | Default | Semantics |
|---|---|---|---|
| `enabled` | Bool | **`true`** (v19: false) | Run the SMD pin-escape pre-pass. NB `getRunFanout()` defaults *true* when null but `isFanoutEnabled()` defaults *false* when null — two different null policies in the same class (`RouterSettings.java:573-580`). |
| `max_passes` | Int | `20` | Safety cap; typical convergence 1–3 passes. |
| `max_items` | Int | `Integer.MAX_VALUE` | Cap on escape attempts. |
| `max_milliseconds_per_pin` | Long | `10000` | Per-pin budget in pass 1; scales as `maxMillisecondsPerPin × passNumber`. |
| `ripup_allowed` (alt `ripupAllowed`) | Bool | `true` | Allow fanout to rip existing traces. |
| `min_escape_length_mm` | Double | `2.5` | Minimum via distance from the pin centre. |
| `max_escape_length_mm` | Double | `4.5` | Search/via-placement radius bound. |
| `start_via_diameter_mm` | Double | `0.250` | Via inside the pad. |
| `end_via_diameter_mm` | Double | `0.250` | Landing via. |
| `pin_sorting_order` | String | `"outer_first"` | `"inner_first"` (= v1.9 order), `"outer_first"`, `"unsorted"` (pin-number order). |
| `fallback_to_board_vias` | Bool | `true` | Use board-wide via rules when the net's via list is empty. |
| `timeout` | String | `null` | Stage timeout; surfaced as `isFanoutTimedOut()`. |

### 3.4 `router.optimizer.*` (`settings/OptimizerSettings.java`)

| Key | Type | Default | Semantics / consumer |
|---|---|---|---|
| `enabled` | Bool | **`true`** in DefaultSettings, but `getRunOptimizer()` returns **`false`** when null | Post-route optimize stage. |
| `algorithm` | String | `"freerouting-optimizer"` | |
| `max_passes` | Int | `100` | Full sweeps over items. |
| `max_items` | Int | `Integer.MAX_VALUE` | `BatchOptimizer.java:169`. |
| `max_threads` | Int | `max(1, cores-1)` | Kept in sync from `RouterSettings.setMaxThreads`. Drives `BatchOptimizerMultiThreaded`. |
| `improvement_threshold` | Float | `0.01` (1 %) | Two uses: early-exit if `scoreBeforePass·(1+t) > …` and stop if `scoreImprovement < t` (`BatchOptimizer.java:182,221`). CLI `-oit` takes **percent** and divides by 100. |
| `max_consecutive_failures` | Int | `50` | Abort current pass (`BatchOptimizer.java:301-351`). |
| `additional_ripup_cost_factor_at_start` | Int | `10` | `ripupCosts *= factor` at optimizer start (`BatchOptimizer.java:456`). |
| `trace_ripup_cost_factor` | Float | `0.6` | `ripupCosts = round(factor · ripupCosts)` when the item being re-routed is a trace (`:462`). |
| `max_autoroute_passes` | Int | `6` | Passes allowed when re-routing one item (`:468`). |
| `board_update_strategy` | enum (transient) | `GREEDY` | `GREEDY` / `GLOBAL_OPTIMAL` / `HYBRID`. CLI `-us global|hybrid|greedy`. |
| `hybrid_ratio` | String (transient) | `"1:1"` | CLI `-hr`. |
| `item_selection_strategy` | enum (transient) | `PRIORITIZED` | `SEQUENTIAL` / `RANDOM` / `PRIORITIZED`. CLI `-is seq|rand|prio`. |
| `timeout` | String | `null` | Stage timeout; `isTimedOut()`. |

### 3.5 `router.scoring.*` (`settings/ScoringSettings.java`)

| Key | Type | Default | Semantics |
|---|---|---|---|
| `unrouted_net_penalty` | Float | `5_000_000.0` | Per incomplete connection. Also the *normalizer*: `maximumScore = maxConnections × unroutedNetPenalty`. |
| `clearance_violation_penalty` | Float | `1_000_000.0` | Per DRC violation. |
| `bend_penalty` | Float | `10.0` | Per trace corner (score only, not the maze). |
| `via_costs` (alt `viaCosts`) | Int | `50` | **Dual use**: maze via cost base *and* score cost term. `setViaCosts` clamps `>=1`. |
| `plane_via_costs` | Int | `5` | Via landing on a plane. `>=1`. |
| `start_ripup_costs` (alt `startRipupCosts`) | Int | `100` | Routing-control only, multiplied by pass number; **not** in the score. `>=1`. |
| `default_preferred_direction_trace_cost` | Double | `1.0` | Per-mm cost in preferred direction; also the trace-length coefficient in `calculateScore`. |
| `default_undesired_direction_trace_cost` | Double | `1.0` | Base before the aspect-ratio add-on (§5). |
| `default_bend_cost` | Double | `0.0` | Fallback for `layers[i].bend_cost`. |
| `preferred_direction_trace_cost[]` | double[] (transient) | derived (§5) | Per-layer. Setter clamps `>= 0.1` and marks `boardSpecificTraceCostsApplied = true`. |
| `undesired_direction_trace_cost[]` | double[] (transient) | derived (§5) | idem. |

### 3.6 Derived/engine-side knobs that are *not* user settings but are part of the surface

Set unconditionally in `AutorouteControl`'s private constructor (`maze/AutorouteControl.java:163-186`):
`isFanout=false`, `removeUnconnectedVias=true`, `tidyRegionWidth=Integer.MAX_VALUE`,
`pullTightAccuracy=500` (**note: hard-coded here, *not* taken from `RouterSettings`**),
`maxShoveTraceRecursionDepth=20`, `maxShoveViaRecursionDepth=5`, `maxSpringOverRecursionDepth=5`,
`attachSmdAllowed=false`, `viaLowerBound=0`, `viaUpperBound=layerCount`, `ripupAllowed=false`,
`ripupCosts=1000`, `ripupPassNo=1`. Per-net in `initNet`: `traceClearanceClassIndex` and `viaRule`
from the net class; `traceHalfWidth[i]` from `BoardRules.getTraceHalfWidth(net, layer)`;
`compensatedTraceHalfWidth[i] = traceHalfWidth[i] + clearanceCompensationValue(traceCl, i)`;
`layerActive[i] &= netClass.isActiveRoutingLayer(i)`.

### 3.7 CLI flags (`settings/GlobalSettings.applyCommandLineArguments`, :521-836)

| Flag | Effect |
|---|---|
| `-de <files…>` | Design input. Accepts several files in one flag, dispatched **by extension**: `.dsn`/`.json` → input (second `.json` becomes the session file), `.ses` → session, `.rules` → rules. Also splits legacy `a.dsn+b.rules`. |
| `-di <dir>` | GUI input directory. |
| `-do <file>` | Output (SES/KiCad-JSON). Presence of both `-de` and `-do` **implies `router.enabled=true`** unless `--router.enabled=` was given (`sources/CliSettings.java:77-83`). |
| `-dr <file>` | Rules file. |
| `-drc <file>` | DRC-only mode: forces `router.enabled=false`, `drc.enabled=true`, disables GUI/API/MCP. |
| `-mp <n>` | max passes, clamped `[0,9999]` (0 = unlimited). |
| `-mt <n>` | optimizer max threads, clamped `[0,1024]`. |
| `-oit <pct>` | optimizer improvement threshold in **percent**. |
| `-us`, `-is`, `-hr` | optimizer strategies / hybrid ratio. |
| `-inc a,b` | ignore net classes (**GUI-only effect**, §3.1). |
| `-l <locale>`, `-dl`, `-da`, `-ll <lvl>`, `-host`, `-dct`, `-help` | locale / disable file log / disable analytics / console level / host tag / dialog timeout / help. |
| `--<path>=<value>` | Generic setter into `GlobalSettings` via reflection; `--router.*` additionally handled by `CliSettings` (priority 60). Special-cased: `--user_data_path=`, `--compare-boards=a,b`, `--debug.*`, `--mcp_server.stdio=`, `--logging.*`. |
| `--router.max_passes=`, `--router.optimizer.max_threads=`, … | Any `RouterSettings` field path, set via `ReflectionUtil.setFieldValue`. Short forms mapped: `-mp`→`router.max_passes`, `-mt`→`router.max_threads` (`CliSettings.java:102-110`). |

Environment: `FREEROUTING__ROUTER__<PATH>` with `__` → `.` (upper-cased before matching), e.g.
`FREEROUTING__ROUTER__OPTIMIZER__MAX_THREADS=4` (`sources/EnvironmentVariablesSource.java:47-99`).
Non-router env vars: `FREEROUTING__USER_DATA_PATH`, `FREEROUTING__LOGGING__{FILE,CONSOLE}__*`,
`FREEROUTING__MCP_SERVER__STDIO`.

---

## 4. The settings pipeline: sources, priorities, merge semantics

```
SettingsSource (interface): getSettings() -> RouterSettings (nullable fields), getSourceName(), getPriority()
```

| Priority | Source | Class |
|---|---|---|
| 0 | hard-coded defaults | `sources/DefaultSettings` |
| 10 | `freerouting.json` (`router` sub-object) in user-data dir | `sources/JsonFileSettings` |
| 20 | DSN `(structure …)` metadata — **layer count** + optional `(autorouteSettings …)` | `sources/DsnFileSettings` |
| 30 | SES metadata | `sources/SesFileSettings` |
| 40 | `.rules` file | `sources/RulesFileSettings` |
| 55 | environment variables | `sources/EnvironmentVariablesSource` |
| 60 | CLI arguments | `sources/CliSettings` |
| 65 | GUI / `WorkspaceSettings` | `sources/GuiSettingsSource` (subclass replaces the placeholder) |
| 70 | REST API / per-job overrides | `sources/ApiSettings` |

> The javadoc in `SettingsSource.java:35-38` says GUI = 50; the actual constant is **65**
> (`GuiSettingsSource.java:36`) and `SettingsMerger`'s class javadoc agrees. Trust the constants.

**Merge algorithm** (`settings/SettingsMerger.merge()`, :133-193):

```
sorted = sources sorted by ascending priority
merged = null
for src in sorted:
    s = src.getSettings();  if s == null: continue
    if merged == null: merged = s.clone()            # first non-null source is the base
    else:              merged.applyNewValuesFrom(s)  # → ReflectionUtil.copyFields
merged.validate()
```

`ReflectionUtil.copyFields(source, target)` (`util/ReflectionUtil.java:215-330`):
* skips `static` and non-`public` fields;
* **wrapper/String/array/object fields: copy iff `sourceValue != null`**;
* **primitive fields: copy iff non-null *and* != the Java default for that primitive** — this is
  why every `RouterSettings` field must be a boxed nullable type with no initializer; a
  non-null default in a low-priority source would silently override a higher-priority source
  (`SettingsMerger` javadoc :22-31);
* arrays of primitives/Strings are copied only when the target array is null or empty;
* arrays of objects: element-wise `copyFields` into existing elements, else new instances;
* enums are re-resolved by `Enum.valueOf(type, source.toString())`.

`addOrReplaceSources` replaces an existing source when the new class is the same **or a subclass**
of an existing source's class — this is how the concrete `WorkspaceSettings` displaces the
placeholder `GuiSettingsSource` after board load (`SettingsMerger.java:107-126`).

`validate()` (`RouterSettings.java:932-965`) is the *only* normalization step: `maxPasses`
(`0 ⇒ MAX_VALUE`, out-of-range ⇒ 9999), `maxThreads` (null/negative ⇒ `cores-1`, `>cores` ⇒ cores),
`tracePullTightAccuracy` (`<1 ⇒ 500`). **It dereferences `this.maxPasses` and
`this.tracePullTightAccuracy` without a null check** — merging a source set with no defaults NPEs.

`RouterSettings.clone()` is a manual deep copy (`:487-524`) that *does not* copy
`resultJsonPath` — a latent bug: a cloned settings object loses the manifest path.
It also calls `setLayerCount()` first, which **resets** `preferredDirectionTraceCost` /
`undesiredDirectionTraceCost` to all-`1.0` and clears per-layer `bendCost` /
`preferredDirectionHorizontal`, before overwriting `layers` from the source — so the cost arrays
of the clone are `1.0` everywhere unless `scoring.clone()` restores them (it does, at `:519`).
Order-dependent and fragile; a rewrite should copy wholesale.

---

## 5. Board-geometry-derived cost initialization (`applyBoardSpecificOptimizations`)

`RouterSettings.applyBoardSpecificOptimizations(board)` (`:266-435`) — identical arithmetic to
`v19:AutorouteSettings(RoutingBoard)`:

```
W = board.boundingBox.width();  H = board.boundingBox.height()
horizAdd = 0.1 * round(10 * W / H)          # extra cost against preferred dir on horiz-preferred layers
vertAdd  = 0.1 * round(10 * H / W)

prefIsHorizontal = (W < H)                  # seed
for i in 0..layerCount-1:
    if layers[i].isSignal: prefIsHorizontal = !prefIsHorizontal     # flip on every SIGNAL layer
    if !isSignal:            layers[i].routable = false
    elif routable == null:   layers[i].routable = true
    if bendCost == null:                 bendCost = scoring.defaultBendCost ?? 0.0
    if preferredDirectionHorizontal==null: = prefIsHorizontal
    if initializeTraceCosts:
        pref[i]      = scoring.defaultPreferredDirectionTraceCost        # 1.0
        undesired[i] = scoring.defaultUndesiredDirectionTraceCost        # 1.0
        undesired[i] += prefIsHorizontal ? horizAdd : vertAdd

if initializeTraceCosts and signalLayerCount > 2:
    outerAdd = 0.2 * signalLayerCount
    pref[0] += outerAdd; pref[last] += outerAdd
    undesired[0] += outerAdd; undesired[last] += outerAdd
boardSpecificTraceCostsApplied = true
```

Gotchas worth preserving:
* The flip happens **before** the assignment, so layer 0 (if signal) gets `!(W<H)`.
* `initializeTraceCosts` is suppressed once any explicit `set{Preferred,AgainstPreferred}DirectionTraceCosts`
  call has run (they set `boardSpecificTraceCostsApplied = true`, `:770, :852`), or once a previous
  invocation completed. `setLayerCount(n)` and array-length mismatches reset the flag to `false`.
* `applyBoardSpecificOptimizationsIfNeeded` re-runs only when the layer count differs or the flag
  is not set (`:245-254`).
* `getHorizontalTraceCosts(l)` / `getVerticalTraceCosts(l)` map (pref, undesired) → (h, v) by
  `preferredDirectionHorizontal`; `getTraceCosts()` materializes an
  `ExpansionCostFactor[]` **fresh on every call** (`:877-889`) — called once per net setup.

---

## 6. The headless pipeline, end to end

```
Freerouting.main(args)                                         Freerouting.java:899
 ├─ 0. detect --mcp_server.stdio / FREEROUTING__MCP_SERVER__STDIO → System.setOut(System.err)
 ├─ 1. resolve user-data path: default OS path
 │       < FREEROUTING__USER_DATA_PATH < FREEROUTING__LOGGING__FILE__LOCATION (deprecated)
 │       < --user_data_path=…            ; then GlobalSettings.setUserDataPath + lockUserDataPath
 ├─ 2. resolve logging (env then CLI), set log4j2 system properties, force reconfigure
 │       (must precede ANY logging call)
 ├─ 3. GlobalSettings.load()  ← freerouting.json ; regenerate if version mismatch/absent
 ├─ 4. applyNonRouterEnvironmentVariables(); applyCommandLineArguments(args)
 ├─ 5. --compare-boards=a,b ⇒ compareBoardFiles + exit
 ├─ 6. analytics identify / version check thread
 ├─ 7. settingsMergerPrototype = SettingsMerger(Default, JsonFile, Cli(args), Env)
 ├─ 8. optionally start API server (Jetty) / MCP server / Swing GUI
 └─ 9. if !gui && !api && !mcp:
        drcReportFile != null ? initializeDrc(...) : initializeCli(...)
```

`initializeCli` (`Freerouting.java:79-187`):

```
session   = SessionManager.createSession(userId, "Freerouting/<version>")
job       = new RoutingJob(session.id)
job.setInput(initialInputFile)                       # detects format by magic bytes / '{'
job.tryToSetOutputFile(new File(initialOutputFile))  # deletes an existing output file first
merger    = globalSettings.settingsMergerProtype.clone()
merger.addOrReplaceSources(DsnFileSettings(input))   # metadata-only DSN parse (stops after (structure))
if initialRulesFile: job.setRules(...); merger.addOrReplaceSources(RulesFileSettings(...))
job.routerSettings = merger.merge()
job.drcSettings    = globalSettings.drcSettings.clone()
job.state = READY_TO_START
poll every 500 ms until state ∈ {COMPLETED, TERMINATED, TIMED_OUT, CANCELLED}
outputWritten = (output data present) && (state ∈ {COMPLETED, TIMED_OUT}) && write succeeded && size>0
exitCode      = (state==COMPLETED||state==TIMED_OUT) && outputWritten ? 0 : 1
if router.result_json: write RoutingResultManifest
```

`RoutingJobScheduler` (singleton, daemon loop, 250 ms tick, `maxParallelJobs = 5`,
`management/jobs/RoutingJobScheduler.java:44-282`) picks up `READY_TO_START` jobs sorted by
priority and, for DSN/KiCad-JSON inputs only:

```
boardManager = new HeadlessBoardManager(job)
boardManager.loadFromSpecctraDsn(...) | loadFromKiCadJson(...)     # full parse
job.board = boardManager.getRoutingBoard()
merger = prototype.clone()
   + DsnFileSettings(input)                       (DSN only)
   + RulesFileSettings(job.rules ?? CLI rules ?? "<designName>.rules" adjacent to the DSN)
   + ApiSettings(job.routerSettings)              # keeps per-job overrides at the top priority
job.routerSettings = merger.merge()
if rules present: RulesReader.read(rules, designName, job.board, job.routerSettings)   # mutates BOTH
job.routerSettings.applyBoardSpecificOptimizations(job.board)
optionally import a SES / KiCad-JSON session on top of the board
job.thread = new RoutingJobSchedulerActionThread(job); job.state = RUNNING; thread.start()
```

`HeadlessBoardManager` post-load, in order (`:711-757`):
1. `applyRouterSettingsForLoadedBoard()` → `setLayerCount` if mismatched,
   `applyBoardSpecificOptimizations`, `applyCopperToEdgeClearanceOverride`,
   `applyHoleClearanceOverride`.
2. `applyImmediatePostLoadProcessing()` → `board.reduceNetsOfRouteItems()` (§10.5) and
   `validatePowerPlanes()` (warns about traces on non-signal layers, planes with no conduction
   area, and overlapping same-layer conduction areas; `:912-1010`).
3. `scheduleDeferredPostLoadProcessing()` — a **virtual thread** that computes
   `BoardStatistics(board, null, false, false)`, fires analytics, computes the CRC32 board
   checksum, and optionally diff-compares against a counterpart board file.

`RoutingJobSchedulerActionThread.threadAction()` (`:37-206`):
timeout arming → daemon monitor thread (1 s tick; samples CPU/alloc/heap via `ThreadMXBean`,
enforces the timeout with a 30 s grace) → `RoutingPipeline.createForHeadless(job).run()` (see
`batch-orchestration.md`) → `setJobOutput(job)` → final state transition
(`RUNNING⇒COMPLETED`, `STOPPING⇒CANCELLED|COMPLETED`).

`setJobOutput` (`:259-295`) picks the output format from the *input* format: KiCad-JSON in ⇒
`KiCadJsonWriter` out (`.json`); otherwise a fresh `HeadlessBoardManager` + `SesWriter` (`.ses`).
It is registered as a **board-updated listener**, so the SES is regenerated on every board update
event during routing — i.e. the full SES serialization runs repeatedly, not once.

---

## 7. core/* — job model, stop protocol, counters, manifest

### 7.1 `RoutingJob` (`core/RoutingJob.java`)

Fields relevant to the router: `id` (UUID), `createdAt`, `shortName` (`SSSSSS\JJJJJJ`),
`state: RoutingJobState`, `stage: RoutingStage`, `priority`, `sessionId`,
`input/output/rules/drc: BoardFileDetails`, `routerSettings`, `drcSettings`, `resourceUsage`,
`currentPass`, transient `thread: StoppableThread`, transient `board: RoutingBoard`,
transient `timeoutAt: Instant`, `isCancelledByUser`.
Input format detection is by content, not extension (`getFileFormat(byte[])`, `:151-…`): first
non-whitespace `{` ⇒ `KICAD_DESIGN_JSON`; `AC ED 00 05` ⇒ `FRB` (Java-serialized board); else
text sniffing for DSN/SES/rules.

### 7.2 States and stages

```
RoutingJobState: INVALID QUEUED READY_TO_START RUNNING PAUSED COMPLETED TIMED_OUT
                 STOPPING CANCELLED TERMINATED
RoutingStage:    IDLE ROUTING OPTIMIZATION
StopRequestState: NONE AUTO_ROUTER_ONLY ALL
```

### 7.3 The stop protocol (`core/StoppableThread.java`, 43 L — small but load-bearing)

```java
requestStop()               -> stopRequestState = ALL                 (synchronized)
isStopRequested()           -> state == ALL
requestStopAutoRouter()     -> if state == NONE then AUTO_ROUTER_ONLY
isStopAutoRouterRequested() -> state != NONE
```

Two-level cancellation: "stop the router but let the optimizer finish" vs "stop everything".
Every long-running loop in the pipeline polls one of these two predicates; there is no
interrupt-based cancellation and no cancellation token passed down — the thread object itself is
the token (`datastructures/Stoppable`). `TimeLimit` (`datastructures/TimeLimit.java`, 32 L) is the
orthogonal wall-clock budget handed to the maze search.

### 7.4 `RouterCounters` (per-pass progress, all boxed `Integer`)

`passCount`, `queuedToBeRoutedCount`, `routedCount`, `skippedCount`, `rippedCount`,
`failedToBeRoutedCount`, `incompleteCount`, `phase` (`"autoroute"`/`"fanout"`),
`fanoutExtraViasCount`.

### 7.5 `RouterJobResourceUsage`

`cpuTimeUsed` (s, from `ThreadMXBean.getThreadCpuTime` of the *job thread only*),
`maxMemoryUsed` (MB, cumulative allocated bytes of the job thread),
`peakMemoryUsed` (MB, peak process heap). Worker threads report separately.

### 7.6 `RoutingResultManifest` (schema v1, `core/results/`)

`schema_version`, `generated_at`, `app_version`, `git_sha`, `fixture{filename, sha256}`,
`settings_snapshot: RouterSettings`, `phases{fanout,autorouter,optimizer}{duration_seconds,
passes_completed}`, `board_statistics: BoardStatistics`, `normalized_score`, `resource_usage`,
`final_state`, `exit_code`, `output_written`. **This is the machine-readable contract a Rust
rewrite should emit byte-compatibly if benchmark harnesses are to be reused.**

---

## 8. Scoring: `BoardStatistics` field-by-field

The score *formula* is in `batch-orchestration.md` §6. What follows is the **data model and how
each number is produced**, because a rewrite has to reproduce the inputs, not just the formula.

Constructor `BoardStatistics(board, unit, includeClearanceViolations, includeConnections)`
(`core/scoring/BoardStatistics.java:110-427`).

| Group | Fields | Derivation |
|---|---|---|
| `board` | `boundingBox`, `size` | From `board.getBoundingBox()`. NB `boundingBox` is filled as `(ur.x, ur.y, ll.x, ll.y)` into a `Rectangle2D.Float(x,y,w,h)` — the "width/height" slots hold **lower-left coordinates**. Cosmetic bug; `size` is correct. |
| `layers` | `totalCount`, `signalCount` | `layerStructure`. |
| `items` | `totalCount`, `traceCount`, `viaCount`, `conductionAreaCount`, `drillItemCount`, `pinCount`, `componentOutlineCount`, `otherCount` | One pass over `itemList`; **dispatch order matters**: `Trace → Via → ConductionArea → Pin → DrillItem → ComponentOutline → other`, so `Pin` is counted before generic `DrillItem`. |
| `components`, `pads`, `nets` | `totalCount`, `classCount` | `components.count()`, `getPins().size()`, `nets.maxNetNumber()`, `netClasses.count()`. |
| `traces` | `totalCount`, `totalLength` (board units), `totalLengthMm`, `averageLength`, `totalSegmentCount`, `totalHorizontalLength`, `totalVerticalLength`, `totalAngledLength`, `totalWeightedLength` | `boardUnitToMmFactor = Unit.scale(1, board.unit, MM) / max(resolution,1)`. Segment count = `cornerCount-1`. H/V/angled classified by exact `a.x==b.x` / `a.y==b.y` on the **defining lines**, not the corners. `totalWeightedLength = Σ length·(halfWidth + clearance(traceClass, defaultClass, layer))` over UNFIXED and SHOVE_FIXED traces only, **halved for SHOVE_FIXED**. |
| `connections` | `maximumCount`, `incompleteCount` | Only when `includeConnections`; from a **fresh** `DesignRulesChecker(board,null).calculateAllIncompletes()`. |
| `bends` | `totalCount`, `ninetyDegreeCount`, `fortyFiveDegreeCount`, `otherAngleCount` | `totalCount += cornerCount-2` per polyline trace (every internal corner counts as a bend, even a straight-through one). Classification by `atan2` difference normalized to `[0,180]`, tolerance ±1°: 90 ⇒ ninety; 45 or 135 ⇒ fortyFive; else other. |
| `vias` | `totalCount`, `throughHoleCount`, `blindCount`, `buriedCount` | through = spans layer 0..last; blind = touches exactly one outer layer; else buried. |
| `clearanceViolations` | `totalCount`, `minViolationUm`, `maxViolationUm`, `avgViolationUm` | Only when `includeClearanceViolations`; from `DesignRulesChecker.getAllClearanceViolations()`; shortfall = `max(0, expected − actual)` scaled to µm. |
| `fanout` | `totalSmdPins`, `pinsToEscape`, `escapedCount` | Over `board.getSmdPins()` with `netCount()>0`. `pinsToEscape = total − alreadyConnected` where *alreadyConnected* means `pin.getUnconnectedSet(net).isEmpty()`. `escapedCount` uses `isPinEscaped`. |

`isPinEscaped(pin)` (`:555-576`) — a pin counts as escaped when it has a normal contact that is
(a) a `Trace` with **no** clearance violations, or (b) a `Via` with no clearance violations that
itself contacts a Trace or ConductionArea, or (c) a `ConductionArea`.

Unit conversion at the end rescales `board.boundingBox`, `board.size` and every trace length field
from `board.communication.unit` to the requested unit — but **not** `totalLengthMm` (already mm)
and **not** the violation µm fields.

---

## 9. The board item model: `Item`, fixed states, the obstacle matrix

### 9.1 `Item` (abstract base, `board/model/items/Item.java`, 1299 L)

```
Item implements SearchTreeObject, ItemInfoPrinter.Printable, UndoableObjects.Storable, Serializable
  final int id                      # from board.communication.idGenerator.newId() if <= 0
  transient BasicBoard board        # back-pointer; nulled on serialize, restored on undo/redo
  double smallestClearance = -1.0   # memo of the tightest clearance seen in clearanceViolations()
  protected int componentId         # 0 == not part of a component
  public int[] netNumbers           # copied defensively in the ctor
  private int clearanceClassIndex
  private transient ItemSearchTreesInfo searchTreesInfo   # per-tree leaves + precalculated shapes
  private FixedState fixedState
  private boolean onTheBoard
  private transient ItemAutorouteInfo autorouteInfo
```

Invariants:
* `compareTo` is `other.id - this.id` — **descending id order** in every `TreeSet<Item>`.
  A rewrite that sorts ascending will change iteration order and therefore results.
* `id` is unique and monotonic per board; `enforceStrictDrc` and several diagnostics rely on
  "id > maxItemIdBefore ⇒ created by this operation" (`pipeline/BatchAutorouter.java:305-329`).
* `netNumbers` is a tiny unsorted array; `containsNet` returns false for `netNumber <= 0`
  (net 0 = "no net", net numbers are 1-based).
* `clearDerivedData()` drops precalculated tree shapes **and** `autorouteInfo`.
* `getTreeShape(tree, i)` self-heals: on a null/short cache it calls `clearDerivedData()` and
  retries once (`:212-226`).

### 9.2 `FixedState` — 4 ordered levels

```
UNFIXED  <  SHOVE_FIXED  <  USER_FIXED  <  SYSTEM_FIXED
```
(`board/model/structure/FixedState.java`; the inline comments on `SHOVE_FIXED`/`USER_FIXED` are
swapped in both trees — ignore them, trust the ordinal ordering.)

| Predicate | Definition |
|---|---|
| `isShoveFixed()` | `ordinal >= SHOVE_FIXED`. `Trace` **overrides** it to also return true when any of its nets belongs to a net class with `shoveFixed` (`Trace.java:238-253`). |
| `isUserFixed()` | `ordinal >= USER_FIXED`. |
| `isDeletionForbidden()` | `componentId > 0` **or** `isUserFixed()` **or** (`ConductionArea` on a non-signal layer). |
| `isRoutable()` | base `false`; `Trace`: `!isUserFixed() && netCount()>0`; `Via`: analogous. |
| `unfix()` | resets to `UNFIXED` unless `SYSTEM_FIXED`. |

Uses: `removeItems()` refuses `isDeletionForbidden() || isUserFixed()` and returns `false`
(`BasicBoard.java:636-647`); `removeItem()` silently no-ops on `isDeletionForbidden()`
(`BoardItemRepository.java:189-191`); `combineAtStart/End` require **identical** `fixedState`
between the two traces plus both deletable; `insertTraceWithoutCleaning` allows a degenerate
(first==last corner) trace only when `fixedState >= USER_FIXED`
(`BasicBoard.java:191-195`); `BoardStatistics` weights SHOVE_FIXED traces at half length;
`Item.isFanoutVia` treats a 2-corner SHOVE_FIXED trace as an SMD exit stub.

### 9.3 The obstacle predicate matrix

`isObstacle(Item other)` is the pairwise DRC gate. `isTraceObstacle(int net)` and
`isDrillable(int net)` are the routing-side gates.

| Item type | `isObstacle(other)` | `isTraceObstacle(net)` | `isDrillable(net)` |
|---|---|---|---|
| `Trace` | `false` if `other` is self / `ViaObstacleArea` / `ComponentObstacleArea` / a non-obstacle `ConductionArea`; else `!other.sharesNet(this)` (`Trace.java:92-102`) | `!containsNet(net)` (base) | `containsNet(net)` |
| `Pin` | `false` if self or any `ObstacleArea`; `true` if `!sharesNet`; same-net: extra rules (trace/via attach) (`Pin.java:353+`) | base | base; `drillAllowed()` = SMD pin (`firstLayer()==lastLayer()`) |
| `Via` | analogous to Pin | base | base |
| `ObstacleArea` (keepout) | `false` if same net; else `other instanceof Trace \|\| other instanceof Via` (`ObstacleArea.java:175-180`) | base | base (`false`) |
| `ViaObstacleArea` | `false` if same net; else `other instanceof Via` only | **`false`** — never blocks traces | base |
| `ComponentObstacleArea` | `other != this && other instanceof ComponentObstacleArea && other.componentId != this.componentId` — **only blocks other components' placement keepouts** | **`false`** | base |
| `ComponentOutline` | **always `false`**, `tileShapeCount() == 0` — pure metadata, invisible to routing | — | — |
| `ConductionArea` | `isObstacle ? super(ObstacleArea rule) : false` | `isObstacle && !containsNet(net)` | `!isObstacle \|\| containsNet(net)` |
| `BoardOutline` | outline handled specially by the search tree (see search-tree-obstacles.md §3.6) | | |

`Item.clearanceViolations()` (`:367-469`) is the DRC oracle and has one non-obvious exception:
two traces of **different** nets are *not* a violation if they both end at a common `Pin` that
shares a net with each of them ("tie pin", `:383-413`). Clearance is then measured by a 16-step
binary search on symmetric `enlarge()` (`calculateClearanceBetweenTwoShapes`, `:471-493`) — i.e.
the reported "actual clearance" is an approximation to ~`minimumClearance/65536`.

---

## 10. Connectivity primitives: contacts, connected sets, connections, cycles, tails

These are used everywhere (ratsnest, ripup, tail removal, combine, cycle removal) and no sibling
spec defines them.

### 10.1 Contact points

* **Trace** connection points = its two end corners.
* **DrillItem** (pin/via) connection point = its centre.
* **ConductionArea** connection points = any point of the area that contains the other item's
  connection point.

`Trace.getNormalContacts(Point p, boolean ignoreNet)` (`Trace.java:173-203`): returns `{}` unless
`p` equals `firstCorner()` or `lastCorner()`; queries `board.overlappingObjects(TileShape.of(p),
layer)`; keeps items that share a layer and (unless `ignoreNet`) share a net, **and** whose own
connection point equals `p`.

`Trace.getNormalContacts()` = union over both end corners.
`Trace.getStartContacts()` / `getEndContacts()` = the two halves.

`Item.getAllContacts()` (`:499-519`) is the looser variant: any `Connectable` item sharing a net
that *overlaps any tile shape*, regardless of endpoint alignment.

`normalContactPoint(other)` returns a point only when the contact is **unique**; for two traces on
the same layer, if they touch at *both* corners the result is `null`
(ambiguous ⇒ callers treat it as a fork) (`Trace.java:130-152`).

### 10.2 Connected set

`Item.getConnectedSet(netNumber, stopAtPlane)` (`:596-635`) — DFS over `getNormalContacts()`,
filtered to `containsNet(netNumber)` when `netNumber > 0`; with `stopAtPlane` it does not traverse
*through* a `ConductionArea` that has `componentId <= 0`. Result is a `TreeSet<Item>` (descending id).

`getUnconnectedSet(netNumber)` = all connectable items of the net minus `getConnectedSet`.

### 10.3 Connection (ripup unit)

`Item.getConnectionItems(StopConnectionOption)` (`:702-781`) — "all traces and vias from this item
until the next fork or terminal item". Options: `NONE`, `VIA` (stop at any via), `FANOUT_VIA`
(stop at a via that `isFanoutVia`). Walk rules:
* start from each unique normal contact whose contact point is unambiguous;
* if `this` is a Trace, require exactly **one** contact at that point (else skip that branch);
* advance while `currentItem.isRoutable()`; at each step collect the contacts, drop the one at the
  previous contact point/layer, and require exactly one remaining ⇒ otherwise fork/stub, stop.

`isFanoutVia(ignoreItems)` (`:1206-1238`): true if a normal contact is an SMD pin with ≤1 contact,
or a short trace (`length < PROTECT_FANOUT_LENGTH(=400) × halfWidth`) that leads to such a pin, or
a 2-corner `SHOVE_FIXED` trace (a pin-exit stub).

### 10.4 Cycles and tails

`Trace.isCycle()` (`:272-330`): `true` immediately if `isOverlap()` (start and end contact sets
intersect); else DFS from the start contacts (pre-seeding `visitedItems` with all start contacts so
the search cannot come back through them) looking for `this`. `ignoreAreas` comes from
`netClass.getIgnoreCyclesWithAreas()`.

`Trace.isTail()` = either end has **no** contacts.

`BasicBoard.removeIfCycle(trace)` (`:1335-1366`, semantics unchanged from v19): if the trace is a
cycle, snapshot whether a *tail* exists at each of its two end corners, remove the trace's whole
`getConnectionItems()` set, then remove any tail that appeared where there was none before.

`BasicBoard.getTraceTail(location, layer, nets)` (`:1306-…`): a same-net trace whose first or last
corner equals `location` and which has zero contacts at that end.

`RoutingBoard.removeTraceTails(net, stopOption)` (`:1193-1238`): collect routable single-net items
that `isTail()` (skipping vias per `stopOption`); for each, if it has exactly one normal contact
take its whole `getConnectionItems(stopOption)`, else just the item; remove them all and then
`combineTraces(net)`.

`RoutingBoard.containsTraceTails(items, exceptNets)` (`:1176-1187`) — used by shove/ripup to
reject a board state.

### 10.5 Net reduction on load

`RoutingBoard.reduceNetsOfRouteItems()` (`:1284-…`): for every non-`SYSTEM_FIXED` item with
`netNumbers.length > 1`, drop each net number that is not present on **all** of its contacts
(vias) / on the pins at each end (traces). Loops until fixpoint. Run once immediately after board
load. This is what makes multi-net "tie" items collapse to their real net.

---

## 11. (b) Trace insert / split / merge — the board-level contract

> `pull-tight-insert.md` §7 documents the **intra-trace** algorithms (`split(clip)`, `combine()`,
> `change()`, `normalize()`) against the v19 sources. This section documents the **board-level**
> contract: which entry point does what, when splitting is triggered by *other* item insertions,
> the convergence guards, and the v2 line references + deltas.

### 11.1 The three trace insertion entry points

```java
// 1. raw — no cleanup, returns the new item (used by split(), shove, and the connection inserter)
PolylineTrace BasicBoard.insertTraceWithoutCleaning(
        Polyline, layer, halfWidth, int[] netNos, clearanceClass, FixedState)      // :178-202

// 2. cleaning — inserts then normalizes against the current changed-area clip
void BasicBoard.insertTrace(Polyline, layer, halfWidth, nets, clearanceClass, fixed) // :209-242

// 3. convenience — points -> Polyline -> (2)
void BasicBoard.insertTrace(Point[], layer, halfWidth, nets, clearanceClass, fixed)  // :248-262
```

`insertTraceWithoutCleaning` contract:
1. `polyline.cornerCount() < 2` ⇒ return `null` (nothing inserted).
2. Construct `PolylineTrace(polyline, layer, halfWidth, nets, clClass, id=0, componentId=0, fixed, board)`.
3. **Degenerate guard**: if `firstCorner().equals(lastCorner())` and `fixedState < USER_FIXED`
   ⇒ return `null` without inserting. (USER_FIXED zero-length traces *are* inserted, which is why
   `PolylineTraceNormalization` needs the matching skip at `:107-121`.)
4. `insertItem(newTrace)` — see 11.2.
5. If `netsNormal()` (all net numbers are real nets), update the board-wide
   `maxTraceHalfWidth` / `minTraceHalfWidth` (initial values `1000` / `10000`, `BasicBoard.java:106-109`).

`insertTrace` (variant 2) additionally:
```
clip = (board instanceof RoutingBoard && changedArea != null) ? changedArea.getArea(layer) : null
try { newTrace.normalize(clip) }
catch (Exception e) { warn "trace segment could not be normalized and was skipped"; continue }
```
**v2 delta:** v19 propagated the exception (`v19:BasicBoard.insert_trace` had no try/catch and
`normalize_traces` was `throws Exception`); v2 swallows it, logs, and leaves the net possibly
unrouted (`BasicBoard.java:230-241`).

### 11.2 `insertItem` / `removeItem` — the mutation protocol

`BoardItemRepository.insertItem` (`:135-167`):
```
if clearanceClassIndex out of range: warn + force 0
item.board = board
board.itemList.insert(item)          # undo bookkeeping, §13
board.searchTreeManager.insert(item) # all active trees
observers.notifyNew(item)
board.additionalUpdateAfterChange(item)   # RoutingBoard: maintain the autoroute DB if active
board.incrementRevision()
```
`removeItem` (`:170-201`): **no-op if `isDeletionForbidden()`**; otherwise
`additionalUpdateAfterChange` → `searchTreeManager.remove` → `itemList.delete` →
`observers.notifyDeleted` → `incrementRevision`.

Note the asymmetry: on insert `additionalUpdateAfterChange` runs *after* the tree insert, on
remove *before* the tree removal.

### 11.3 Splitting triggered by inserting a drill item

Inserting a via splits same-net traces at the via centre on every layer the via spans:

```java
// BasicBoard.insertVia  (:268-295)
insertItem(newVia)
for layer in [padstack.fromLayer() .. padstack.toLayer())      // NOTE: half-open, excludes toLayer
    for net in netNumbers: splitTraces(center, layer, net)

// BasicBoard.insertEscapeVia (:310-330)  — v2 only
newVia.isEscapeVia = true; newVia.escapeViaSmdLayer = smdLayer
insertItem(newVia)
for layer in [fromLayer .. toLayer]                            // NOTE: CLOSED interval
    for net in netNumbers: splitTraces(center, layer, net)
```
The half-open loop in `insertVia` is inherited verbatim from `v19:BasicBoard.java:306` — a trace
on the via's **last** layer is not split. `insertEscapeVia` uses `<=`. Preserve both (or
deliberately fix and note the divergence).

`insertPin` does **not** split traces at all (`:336-346`).

`BasicBoard.splitTraces(Point location, int layer, int net)` (`:891-907`):
```
picked = pickItems(location, layer, filter=TRACES)
locationShape = TileShape.getInstance(location).boundingOctagon()
for each picked trace containing net:
    pieces = trace.split(locationShape)       # the clip-shape overload
    if pieces.size() != 1: traceSplit = true
return traceSplit
```

### 11.4 `PolylineTrace.split(IntOctagon clip)` — v2 references and deltas

v2: `board/trace/PolylineTrace.java:465-691`. Algorithm as in `pull-tight-insert.md` §7.1.
Additional v2-only details worth having:

* Early exit: `if (!netsNormal()) { return [this]; }` — items on pseudo-nets are never split (`:467-471`).
* Loop bound is `lines.lines.length - 2` (the polyline stores `n+1` defining lines for `n` corners;
  the first and last are the bounding lines).
* Per segment: `clip.intersects(segment.boundingBox())` prescreen, then
  `defaultTree.overlappingTreeEntries(shape, layer, out)`.
* Self-intersection skip window is `shapeIndexInObject ∈ [i-1, i+1]`, plus the zero-length
  intermediate-segment corner-equality test (`:498-511`).
* After splitting a *found* trace, the overlapping-entry list is **re-queried and the iterator
  reset** because the tree changed (`:583-590`).
* Cycle removal order after any split: **found-trace pieces first, then own pieces**
  ("to preserve them, if possible", `:646-648`).
* `ConductionArea` case: if the trace contacts the same plane at both ends and the net class does
  not set `ignoreCyclesWithAreas`, the trace is removed outright and `split` returns early (`:661-675`).
* If `result.size() > 1`, `board.additionalUpdateAfterChange(piece)` is called for every piece (`:685-689`).

`split(int lineIndex, Line newEndLine)` — the primitive (`:719-760`):
```
if !isOnTheBoard(): return null
if isDeletionForbidden(): return null                      # v2-only guard
pieces = lines.split(lineIndex, newEndLine)                # null or exactly 2 polylines
if splitInsideDrillPadProhibited(lineIndex, newEndLine): return null
board.removeItem(this)
r[0] = board.insertTraceWithoutCleaning(pieces[0], layer, halfWidth, nets, clClass, fixedState)
r[1] = board.insertTraceWithoutCleaning(pieces[1], …)
return r                                                   # entries may individually be null
```

`splitInsideDrillPadProhibited(lineIndex, line)` (`:768-792`): compute the exact intersection
point; over `board.pickItems(intersection, layer, null)`, considering same-net items only:
* a `Pin` whose centre equals the intersection ⇒ **allow** the split (return false);
* any other `Pin` overlapping ⇒ remember `padFound`;
* a `Trace` whose first/last corner equals the intersection ⇒ **allow**;
return `padFound`. Vias are intentionally excluded ("extending the function to vias led to broken
connection problems", comment `:764-766`).

> **Operator-precedence bug preserved in both trees** (`v2 PolylineTrace.java:785-786`,
> `v19 :645-646`):
> `if (currentTrace != this && currentTrace.firstCorner().equals(x) || currentTrace.lastCorner().equals(x))`
> — `&&` binds tighter than `||`, so a *self* match at `lastCorner` also allows the split. Replicate
> deliberately or document the divergence.

`Trace[] split(Point p)` (`:699-712`): find the segment containing `p`, split with the line through
`p` in the direction `segment.direction().turn45Degree(2)` (i.e. perpendicular).

### 11.5 `combine()` — v2 references and deltas

v2: `PolylineTrace.java:175-192` (driver), `:201-332` (`combineAtStart`), `:341-…` (`combineAtEnd`).

```
combine():
  changed = false
  while isOnTheBoard() && (combineAtStart(true) || combineAtEnd(true)):
      changed = true
      observers.notifyChanged(this)
      board.additionalUpdateAfterChange(this)
  return changed
```
**v2 delta:** iterative loop replaces v19's recursion (`v19:PolylineTrace.java:185-205`), which
overflowed the stack on long collinear chains from SES round-trips.

`combineAtStart(ignoreAreas=true)` preconditions — all must hold:
1. `getNormalContacts(firstCorner(), false)` minus `ConductionArea`s has **exactly one** element;
2. it is a `PolylineTrace` with the **same layer**, `netsEqual`, **same half width**, **same
   `fixedState`**, and (v2 only) `!other.isDeletionForbidden() && !this.isDeletionForbidden()`;
3. that trace ends (`lastCorner`) or starts (`firstCorner` ⇒ `reverseOrder`) at the corner.

Join:
```
otherLines = reverseOrder ? reverse(other.lines).map(opposite) : other.lines
skipLine   = otherLines[len-2].isEqualOrOpposite(thisLines[1])       # exact collinear join
newLineCount = thisLines.len + otherLines.len - 2 - (skipLine ? 1 : 0)
newLines = otherLines[0 .. len-2] ++ thisLines[1 ..]                 # at joinPos
joined = new Polyline(newLines)
if joined.lines.length != newLineCount || !bothHaveDefaultTreeEntries:
     PolylineTraceSearchTreeAdapter.replaceGeometry(this, joined)    # remove + reinsert (safe path)
else:
     PolylineTraceSearchTreeAdapter.mergeEntriesInFront(other, this, joined, otherLines.len-3, toNo)
     other.clearSearchTreeEntries(); this.lines = joined             # fast path, reuses tree leaves
if this.lines.length < 3: board.removeItem(this)
board.removeItem(other)
routingBoard.joinChangedArea(startCorner, layer)
```
**v2 delta:** the `!hasTreeEntries` fallback (`:304-311`) — v19 would NPE inside
`merge_entries_in_front` when a trace had no default-tree entries.

`board.itemList.saveForUndo(this)` is called **before** the geometry mutation (`:279`).

### 11.6 `normalize` and the board-level convergence guards

`PolylineTrace.normalize(clip)` → `PolylineTraceNormalization.normalize(trace, clip, 0)`
(`board/trace/PolylineTraceNormalization.java`, `MAX_NORMALIZATION_DEPTH = 16`):

```
if depth > 16: log debug, return false                    # v19 THREW here
observersActivated = !board.observersActive(); if so board.startNotifyObservers()
pieces = trace.split(clip);  result = (pieces.size() != 1)
for piece in pieces where piece.isOnTheBoard():
    combined = piece.combine()
    if piece.cornerCount()==2 && first==last:
        if !piece.isDeletionForbidden(): board.removeItem(piece); result = true
        else: log debug and SKIP        # v2-only: prevents an infinite outer loop
    elif combined:
        normalize(piece, clip, depth+1); result = true
if observersActivated: board.endNotifyObservers()
return result
```

`BasicBoard.normalizeTraces(net)` (`:709-795`):
```
if net ∈ normalizeSuppressedNetNos: log + return false          # v2-only
iteration = 0
while somethingChanged:
    if ++iteration > MAX_NORMALIZE_ITERATIONS (=2000):
        warn; normalizeSuppressedNetNos.add(net); break         # v2-only oscillation valve
    somethingChanged = false
    netTraces = snapshot of on-board PolylineTraces of this net   # collected FIRST (v2-only) to
                                                                  # avoid CME during mutation
    on ConcurrentModificationException while collecting: somethingChanged = true; retry
    for t in netTraces where t.isOnTheBoard():
        if t.normalize(null):                       somethingChanged = result = true
        elif !t.isUserFixed() && removeIfCycle(t):  somethingChanged = result = true
```
v19 iterated the live `itemList` directly and simply restarted the pass on a CME
(`v19:BasicBoard.java:888-919`), with no iteration cap.

`normalizeAllTraces()` (`:798-885`, **v2 only**) is the batched variant used on board load: group
all traces by net once, then run the same inner loop per net, re-collecting after each changed
sweep.

`combineTraces(net)` (`:683-706`): repeatedly scan `itemList` for on-board traces of the net (or
all nets when `net < 0`) and call `combine()`; **restart the scan from the beginning after every
successful combine** (`break` out of the inner loop). O(n²) by construction. Used after ripup and
after `removeTraceTails`.

### 11.7 Insert/merge call graph

```
Callers of insertTraceWithoutCleaning:
  PolylineTrace.split(lineIndex, line)                 (both pieces)
  autoroute/path/FoundConnectionInserter               (via RoutingBoard.insertForcedTracePolyline)
  board/facade/RoutingBoard.insertForcedTracePolyline  (shove core; pull-tight-insert.md §4.6)
  io/specctra/SesReader, io/kicad/KiCadJsonReader      (session import)
  board/optimize/TraceTightener*                       (via PolylineTrace.change)

Callers of insertTrace (cleaning):
  io/specctra parser Wiring/Network (board load)
  interactive route commit paths (GUI)

Callers of normalizeTraces / normalizeAllTraces:
  board load (after all wiring is inserted)
  RoutingBoard.optChangedArea + ripup paths
  BatchOptimizer per-item re-route

Callers of combineTraces:
  RoutingBoard.removeTraceTails, ripup cleanup, BatchAutorouter end-of-pass
```

---

## 12. Areas: ObstacleArea, ViaObstacleArea, ComponentObstacleArea, ConductionArea, BoardOutline

Class hierarchy: `Item → ObstacleArea → {ViaObstacleArea, ComponentObstacleArea, ConductionArea}`;
`ConductionArea implements Connectable`. `ComponentOutline` and `BoardOutline` extend `Item` directly.

### 12.1 `ObstacleArea` (generic keepout)

* Geometry: a `relativeArea: Area` plus `translation: Vector`, `rotationInDegree: double`,
  `sideChanged: boolean`. `getArea()` materializes the transformed area lazily and caches it.
* `tileShapeCount()` / `getTileShape(i)` are derived from `splitToConvex()` = `getArea().splitToConvex()`;
  **null on a degenerate area ⇒ `tileShapeCount()` returns 0** (silently invisible to the tree).
* `shapeLayer(i) == layer` for all `i` (single-layer item).
* Selection filter category: `KEEPOUT`.

### 12.2 `ViaObstacleArea`

Blocks **vias only** (`isObstacle` requires `other instanceof Via`); `isTraceObstacle()` is hard
`false`. Filter category `VIA_KEEPOUT`.

### 12.3 `ComponentObstacleArea`

Blocks **only other components' placement keepouts** (`other instanceof ComponentObstacleArea &&
other.componentId != this.componentId`); `isTraceObstacle()` hard `false`. It is therefore
completely invisible to the router and exists for component placement only. Filter category
`COMPONENT_KEEPOUT`.

### 12.4 `ConductionArea` (copper pour / plane)

```java
private boolean isObstacle;    // "regarded as obstacle to foreign-net traces and vias"
private boolean isFilled = true;
```
* `isObstacle(other)` delegates to `ObstacleArea`'s rule **only when `isObstacle` is true**;
  otherwise foreign nets may route straight through the pour.
* `isTraceObstacle(net) = isObstacle && !containsNet(net)`.
* `isDrillable(net)  = !isObstacle || containsNet(net)` — a non-obstacle pour is always drillable.
* `getNormalContacts()` (`:332-356`): over each convex tile shape, take overlapping same-net
  same-layer items and keep a Trace when the shape **contains** one of its end corners, or a
  DrillItem when the shape contains its centre.
* `getTraceConnectionShape(tree, i) = getTreeShape(tree, i)` — the whole tile is connectable.
* `getRatsnestCorners()` returns **every** approximated corner of the area (expensive for big pours).
* `isDeletionForbidden()` is true for a pour on a non-signal layer (`Item.java:828-830`).
* Board-wide toggle: `RoutingBoard.changeConductionIsObstacle(value)` (`:1253-1277`) flips
  `isObstacle` on all pours on **signal** layers, sets `rules.setIgnoreConduction(!value)`, and
  forces `searchTreeManager.reinsertTreeItems()` if anything changed. The first line is a guard
  (`if (rules.getIgnoreConduction() != value) return;`) that reads like a bug but is
  load-bearing idempotence.
* `BasicBoard.makeConductive(ObstacleArea, net)` (`:1188-…`) upgrades a keepout to a pour.
* `BasicBoard.unfillConductionAreas()` (`:1426-…`) drops the fill.
* The `getDetailedFillArea` / `warmDetailedFillCache` machinery (`:83-234`) is **render-only**
  (java.awt.geom, thermal spokes at ±45°, spoke width `400 µm` in board units, clearance lookup
  floor `2000 µm`). Cached by `board.getRevision()`. A headless rewrite can skip it entirely, but
  note that it is the *only* place thermal-relief geometry exists — the router never models spokes.

### 12.5 `BoardOutline`

Inserted by the `BasicBoard` constructor from `outlineShapes` + `outlineClClassNo`
(`BasicBoard.java:135, 576-580`). The search tree gives it special treatment (see
search-tree-obstacles.md §3.6). `HeadlessBoardManager.applyCopperToEdgeClearanceOverride` may
reassign its clearance class post-load (§3.1).

### 12.6 `ComponentOutline`

`isObstacle → false`, `tileShapeCount() → 0`. Pure documentation geometry.

---

## 13. Undo/redo: `UndoableObjects`

`datastructures/UndoableObjects.java` (343 L). Every board item lives in this structure; it is
also the *item iteration order* source for the whole engine.

```java
ConcurrentMap<Storable, UndoableObjectNode> objects = new ConcurrentSkipListMap<>();
Vector<Collection<UndoableObjectNode>> deletedObjectsStack;   // one list per undo level
int stackLevel; boolean redoPossible;

class UndoableObjectNode { final Storable object; int level;
                           UndoableObjectNode undoObject, redoObject; }
interface Storable extends Comparable<Object> { Object clone(); }  // "must not contain references"
```

* The map is keyed by the object itself and is a **skip-list sorted by `Item.compareTo`**, i.e.
  **descending id**. `startReadObject()` returns `objects.values().iterator()`; `readObject(it)`
  skips nodes with `level > stackLevel` (objects that only exist after a redo). *Every* board scan
  in the codebase goes through this pair.
* `insert(o)`: `disableRedo()`, put a node at the current `stackLevel`.
* `delete(o)`: `disableRedo()`; if a delete list exists for the current level, push either the node
  (when `node.level < stackLevel`) or `node.undoObject`; then `objects.remove(o)`.
* `saveForUndo(o)`: **must** be called before the first mutation of an object after a snapshot.
  If `node.level < stackLevel`, clone the object into a new node at the old level, link
  `undoObject`/`redoObject`, and bump `node.level = stackLevel`.
* `generateSnapshot()`: push a new empty delete list, `++stackLevel`.
* `undo(cancelled, restored)`: for nodes at `stackLevel` replace by `undoObject` (recording both
  lists); re-add every node from `deletedObjectsStack[stackLevel-1]`; `--stackLevel`;
  `redoPossible = true`.
* `redo(...)`: `++stackLevel`; promote `redoObject`s at that level; re-delete this level's delete
  list (following `redoObject` chains).
* `popSnapshot()`: merge the top delete list into the one below, re-link undo/redo pointers, and
  decrement the levels of everything above.
* `disableRedo()`: truncates the delete stack to `stackLevel` and drops all nodes above it.

Board side-effects (`BasicBoard.applyUndoRedoSideEffects`, `:1255-1288`):
```
for o in cancelledObjects: searchTreeManager.remove(o); observers.notifyDeleted(o); changedNets += o.nets
for o in restoredObjects:  o.board = this; searchTreeManager.insert(o);
                           o.clearAutorouteInfo(); observers.notifyNew(o); changedNets += o.nets
```
`BasicBoard.undo/redo` also undo/redo `components` first.

**Separate mechanism — full-board snapshots.** `BoardSnapshotManager`
(`board/facade/BoardSnapshotManager.java`) does Java serialization:
* `serialize(false)` = `ObjectOutputStream.writeObject(board)` (whole board);
* `serialize(true)` = traces + vias + itemList only ("basic profile");
* `BasicBoard.clone()` = serialize(false) → `SafeObjectInputStream` → deserialize;
* `getHash()` = **MD5 of the basic profile** — this is the board hash reported in logs/analytics;
* `diffTraces(other)` = symmetric difference of trace-id sets.

`strictDrc` and `BoardHistory` both use the serialize/deserialize round-trip as their
"transaction". `RoutingBoard.deepCopy()` is the same thing.

---

## 14. The rules model: BoardRules / ClearanceMatrix / NetClass / ViaRule

### 14.1 `BoardRules` (`rules/BoardRules.java`, 435 L)

```java
public final ClearanceMatrix clearanceMatrix;
public final Nets nets;
public final ViaInfos viaInfos;              // all available padstack-based via types
public final Vector<ViaRule> viaRules;       // named subsets of viaInfos
public final NetClasses netClasses;
private transient AngleRestriction traceAngleRestriction = FORTYFIVE_DEGREE;   // default!
private boolean ignoreConduction = true;                                        // default!
private int minTraceHalfWidth = 100000, maxTraceHalfWidth = 100;                // inverted seeds
private double pinEdgeToTurnDist;
private boolean useSlowAutorouteAlgorithm;
private int holeClearance = 0;
```

* `defaultClearanceClass() == 1`, `clearanceClassNone() == 0` (static).
* `getTraceHalfWidth(net, layer)` → `nets.get(net).getNetClass().getTraceHalfWidth(layer)`.
* `createDefaultNetClass()` creates class `"default"` with **trace half width 1500** and trace
  clearance class 1.
* `createDefaultViaRule(netClass, name)`: takes every `ViaInfo` whose clearance class equals the
  net class's default VIA clearance class; when two vias share a layer range, **keep the one with
  the smaller `maxWidth()` pad**.
* `getDefaultViaDiameter()`, `getPinEdgeToTurnDist()` (used by `PolylineTrace.correctConnectionToPin`;
  forced to −1 during connection insertion, see pull-tight-insert.md §8),
  `getTraceAngleRestriction()` (`NONE` / `FORTYFIVE_DEGREE` / `NINETY_DEGREE`, selects the search
  tree and the locator/pull-tight variant).
* `changeClearanceClassIndex(from, to, items)` and `removeClearanceClass(index, items)` renumber
  every item's class index — they must be applied to the search tree too.

### 14.2 `ClearanceMatrix` (`rules/ClearanceMatrix.java`, 454 L)

Storage: `Row[] row`, each `Row` has `MatrixEntry[] column`, each entry has `int[] layer` — i.e. a
dense `classCount × classCount × layerCount` `int` cube, plus `int[] maxValueOnLayer`.

```java
public static final int clearance_safety_margin = 16;        // identical in v19

int getValue(int classI, int classJ, int layer, boolean addSafetyMargin)
    -> row[classJ].column[classI].layer[layer] + (addSafetyMargin ? 16 : 0)
    (returns 0 with a log line on any out-of-range index)

int clearanceCompensationValue(int cl, int layer) = (getValue(cl, cl, layer, false) + 1) / 2
int maxValue(int classI, int layer)  /  int maxValue(int layer)
boolean isLayerDependent(i, j)  /  isInnerLayerDependent(i, j)
void setValue(i, j, value)      // all layers, symmetric
void setValue(i, j, layer, value)
void setInnerValue(i, j, value) // inner layers only
boolean appendClass(String name) // clones the default class's values; false if the name exists
```
The matrix is symmetric by construction of the setters but **read asymmetrically**
(`row[classJ].column[classI]`); a rewrite must keep the same index order to be bit-compatible if
any external file ever writes an asymmetric matrix.

### 14.3 `NetClass` (`rules/NetClass.java`, 282 L)

| Field | Default | Router relevance |
|---|---|---|
| `traceHalfWidthArr[layer]` | from `BoardRules.getNewNetClass` (default class's layer-0 width) | trace width per layer |
| `activeRoutingLayerArr[layer]` | all true | ANDed into `AutorouteControl.layerActive` |
| `traceClearanceClass` | inherited from default class | `AutorouteControl.traceClearanceClassIndex` |
| `viaRule` | default via rule | the via palette for this net |
| `defaultItemClearanceClasses` | per `ItemClass` (`AREA`, `VIA`, …) | used for outline/keepout/via classes |
| `shoveFixed` | false | makes every trace of the net `isShoveFixed()` |
| `pullTight` | **true** | whether the optimizer may tighten this net |
| `ignoreCyclesWithAreas` | false | suppresses plane-cycle removal in `split()` and `isCycle()` |
| `minimumTraceLength` / `maximumTraceLength` | 0 / 0 (= no constraint) | length-violation reporting only (`NetIncompletes.calcLengthViolation`); **not** enforced by the router |
| `isIgnoredByAutorouter` | false | `Item.hasIgnoredNets()` → item skipped in `BatchAutorouter.java:375` |

### 14.4 `Net`, `Nets`, `ViaRule`, `ViaInfo`

`Net`: `name`, `netNumber` (1-based), `subnetNumber`, `containsPlane`, `netClass`;
`getTerminalItems()`, `getPins()`, `getItems()`, `getTraceLength()`, `getViaCount()`.
`Nets.isNormalNetNumber(n)` gates every net-number operation.
`ViaRule` = an ordered list of `ViaInfo`s + `getLayerRange(from,to)`.
`ViaInfo` = `{name, padstack, clearanceClassIndex, attachSmdAllowed}`.

---

## 15. DRC and incompletes (ratsnest)

### 15.1 `NetIncompletes` — the per-net minimum spanning forest

`drc/NetIncompletes.java` (398 L). Algorithm:

```
1. FILTER (v2 only):  drop item if item.isTail()
                      drop item if NOT (ConductionArea | DrillItem) and getNormalContacts().isEmpty()
2. GROUP:   calculateNetItems(filtered) -> NetItem[] {item, connectedSet}
            (repeatedly take an unvisited item, expand getConnectedSet(net), emit one NetItem per
             member sharing that set object)
   connectedGroupCount = |distinct connectedSet identities|
3. if netItems.length <= 1: no incompletes; return
4. Delaunay-triangulate the NetItems (datastructures/PlanarDelaunayTriangulation, 978 L)
5. Sort the triangulation edges ascending by length  (SortedSet<Edge>)
6. KRUSKAL: for each edge in order:
       if from.connectedSet == to.connectedSet (identity!): skip
       emit AirLine(net, fromItem, fromCorner, toItem, toCorner)
       joinConnectedSets(...)   # O(n) rewrite of the connectedSet pointer on every NetItem
7. calcLengthViolation()
```

Notes:
* Edge weight is Euclidean distance between the *ratsnest corners* chosen by the triangulation,
  not centroids. `Item.getRatsnestCorners()`: `Trace` returns only its **uncontacted** end corners;
  `DrillItem` returns its centre; `ConductionArea` returns every corner.
* `drawMarkerRadius = 2 × rules.getMinTraceHalfWidth()`.
* `calcLengthViolation()`: `> 0` too long, `< 0` too short (only when there are no incompletes),
  `0` OK; change threshold `0.1`.
* `joinConnectedSets` is O(n) per merged edge ⇒ O(n²) per net. Replace with union-find.
* **v2 delta**: the step-1 filter does not exist in v19 (`v19:NetIncompletes.java:33-45`), so v2
  reports fewer incompletes on boards with dangling stubs — and since `incompleteCount` is the
  dominant score term (5 M each) and also the `maximumCount` normalizer, scores are not comparable
  across versions.

### 15.2 `DesignRulesChecker` (821 L)

Two entry points used by the router/score path:
* `calculateAllIncompletes()` → sets `maxConnections` and `getIncompleteCount()`;
* `getAllClearanceViolations()` → `Collection<ClearanceViolation>` built from
  `Item.clearanceViolations()` over the whole board.

`ClearanceViolation` carries `{firstItem, secondItem, shape (the intersection), layer,
expectedClearance, actualClearance}`. `AirLine` carries `{net, fromItem, fromCorner, toItem,
toCorner}`. `UnconnectedItems` is a small report DTO.

Cost note: `Item.clearanceViolations()` allocates a `LinkedList`, runs a tree query **per tile
shape**, and does a 16-iteration `enlarge`/`intersection` binary search per violating pair.
Board-wide DRC is therefore expensive, and the score path calls it whenever
`includeClearanceViolations` is set.

---

## 16. The numeric substrate: geometry, `Limits`, and the BigInteger hot path

**This area needs its own spec; here is the scope and the traps.**

`geometry/planar` (11 832 L) is the exact/approximate 2-D kernel:

| Class | Role |
|---|---|
| `Point` (abstract) → `IntPoint`, `RationalPoint` | exact points; `RationalPoint` holds `BigInteger x, y, z` (homogeneous) |
| `Vector` (abstract) → `IntVector`, `RationalVector` | idem for vectors |
| `Direction` (abstract) → `IntDirection`, `BigIntDirection` | normalized direction; `turn45Degree(k)`, `angleApprox()` |
| `Line` | oriented line through two `Point`s; `intersection`, `intersectionApprox`, `sideOf`, `signedDistance` |
| `LineSegment` | a line plus its two bounding lines; `intersection(other) → Line[]`, `contains(Point)` |
| `Polyline` | array of `n+1` defining `Line`s ⇒ `n` corners; `split(index, line)`, `corner(i)`, `cornerApprox(i)` |
| `TileShape` (abstract) → `Simplex`, `IntBox`, `IntOctagon` | convex cells; the search-tree currency |
| `PolygonShape`, `PolylineShape`, `PolylineArea`, `Circle`, `Polygon` | area geometry, `splitToConvex()` |
| `FloatPoint`, `FloatLine` | double approximations used in all the hot inner loops |
| `Limits` | `CRIT_INT = 33554432 = 2^25`, `CRIT_DOUBLE = 2^53`, `CRIT_INT_BIG`, `sqrt2` |

**The invariant that makes it work:** all board coordinates must stay within ±2^25 so that the
product of two coordinates fits exactly in a `double` mantissa with room for one addition. Every
`intersectionApprox`, `sideOf(FloatPoint, tolerance)` and `signedDistance` relies on it.

**The BigInteger hot path.** `Line.intersection(Line other)` (`geometry/planar/Line.java:218-303`)
fast-paths the axis-aligned and ±45° combinations with pure `int` arithmetic, and otherwise falls
through to:

```java
BigInteger det1 = valueOf(a.determinant(b));
BigInteger det2 = valueOf(other.a.determinant(other.b));
BigInteger det  = valueOf(delta2.determinant(delta1));
isX = det1*delta2.x - det2*delta1.x ;  isY = det1*delta2.y - det2*delta1.y
if det != 0 and isX % det == 0 and isY % det == 0 and |isX/det|,|isY/det| <= CRIT_INT:
     return new IntPoint(...)
return new RationalPoint(isX, isY, det)     // BigInteger triple, virally slow downstream
```
That is **8 `BigInteger` allocations + 2 `mod` + 2 `divide` per non-axis-aligned line
intersection**, and it is called from `PolylineTrace.split` (per segment per overlapping entry),
from `splitInsideDrillPadProhibited`, from `Line.sideOfIntersection` (the exact fallback), and from
the 45°/any-angle locators. `RationalPoint` then propagates `BigInteger` arithmetic into
`sideOf`, `distance`, `toFloat`, ... for the rest of that point's life
(125 `BigInteger` references across `geometry/planar` + `datastructures/BigIntAux`).

For 45°-restricted boards (the common case) the fast paths cover almost everything; for
`AngleRestriction.NONE` boards the BigInteger path dominates.

---

## 17. I/O scope: DSN / SES / KiCad-JSON / rules

Not covered by any sibling spec; ~15 kL in v2.

* `io/specctra/parser/SpecctraDsnStreamReader` (1862 L) — hand-written lexer (was a JFlex `.flex`
  in v19: `designforms/specctra/SpecctraFileDescription.flex`).
* `io/specctra/parser/{Structure 1354, Network 1465, Library 473, Wiring 715, Package 425,
  Component 381, Shape 646, Rule 343, PartLibrary 351, NetClass, Circuit, Polygon, PolygonPath,
  Net, Keyword, Parser, DsnFile, ReadScopeParameter, AutorouteSettings, SessionToEagle}`.
* `io/specctra/DsnReader` (291) with a **metadata-only mode** (`readMetadata`, stops after the
  `(structure …)` scope — used by `DsnFileSettings` to learn the layer count cheaply).
* `io/specctra/SesWriter` (552), `SesReader` (413), `RulesReader` (376) / `RulesWriter`.
  `RulesReader.read(stream, designName, board, routerSettings)` mutates **both** the board rules
  and the `RouterSettings` — the only source that does.
* `io/kicad/{KiCadJsonReader 1011, KiCadJsonWriter 227, KiCadBoardJson 142}` — **v2 only**.
* `io/CoordinateTransform` (175) + `board/state/CoordinateTransform` — DSN user units ↔ board
  integer units; `Communication.resolution` and `Communication.unit` are the scale factors that
  every µm-valued setting (§3.1) and every statistic (§8) converts through:
  `boardUnits = round(Unit.scale(valueUm × resolution, UM, board.unit))`.
* `BoardReadResult` is a sealed-ish result type: `Success(board, metadata)`, `OutlineMissing(board)`,
  `ParseError(location, detail)`, `IoError(cause)`.

---

## 18. (c) Things nobody else's spec covers that a rewrite must not forget

1. **Item iteration order is descending id** (`Item.compareTo` = `other.id - this.id`) and every
   board scan uses `UndoableObjects.startReadObject()` over a `ConcurrentSkipListMap` keyed by the
   item. Changing the order changes ripup choices, combine order and therefore the routed result.
   → §9.1, §13.
2. **`readObject(it)` skips nodes with `level > stackLevel`.** A naive `Vec<Item>` replacement
   silently includes redo-only items. → §13.
3. **The trace-insert degenerate guard and the `isDeletionForbidden` guards** are what keep
   `normalizeTraces` from spinning. Both are v2-only; a from-scratch implementation *will* hit the
   infinite loops v2 fixed. → §11.1, §11.6.
4. **`insertVia` splits traces on `[fromLayer, toLayer)` (half-open) but `insertEscapeVia` on
   `[fromLayer, toLayer]`.** → §11.3.
5. **`splitInsideDrillPadProhibited` has an operator-precedence bug** that makes self-matches at
   `lastCorner` permit a split. → §11.4.
6. **Cycle-removal ordering after a split** (found-trace pieces before own pieces) changes which
   trace survives. → §11.4.
7. **The tie-pin exception in `clearanceViolations()`**: two different-net traces meeting at a
   shared pin are not a violation. → §9.3.
8. **`ComponentObstacleArea` and `ComponentOutline` are invisible to routing**; only
   `ObstacleArea` (traces+vias) and `ViaObstacleArea` (vias only) block. Getting this wrong makes
   boards unroutable or produces phantom DRC. → §12.
9. **`ConductionArea.isObstacle` is per-item and board-togglable** (`changeConductionIsObstacle`),
   and `isDrillable` inverts it. Planes are `isDeletionForbidden` on non-signal layers. → §12.4.
10. **Net reduction (`reduceNetsOfRouteItems`) must run once after load**, or multi-net tie items
    poison connectivity. → §10.5.
11. **The µm→board-unit conversion** (`Unit.scale(um × resolution, UM, board.unit)`) applies to
    `copper_to_edge_clearance_um`, `hole_clearance_um`, `neck_width_um`. Off-by-a-factor here is
    silent and catastrophic. → §3.1, §17.
12. **`copper_to_edge` and `hole_clearance` overrides synthesize clearance classes and force a full
    `reinsertTreeItems()`.** The search tree must be rebuilt, not patched. → §3.1.
13. **Layer-cost initialization is aspect-ratio dependent and flips the preferred direction on
    every *signal* layer**, not every layer, and adds `0.2 × signalLayerCount` on the two outer
    layers when there are >2 signal layers. → §5.
14. **`AutorouteControl` hard-codes `pullTightAccuracy = 500`** independent of
    `router.trace_pull_tight_accuracy`; the setting reaches pull-tight through a different path.
    → §3.6.
15. **`getRunFanout()` and `isFanoutEnabled()` disagree on the null default** (true vs false), and
    `getRunOptimizer()` defaults to false while `DefaultSettings` sets it true. Reproduce the
    resolved values, not the accessor quirks. → §3.3, §3.4.
16. **`-inc` / `ignore_net_classes` is a no-op in headless mode.** Only the GUI writes
    `NetClass.isIgnoredByAutorouter`. If parity with the CLI is the goal, keep it a no-op; if
    parity with intent, wire it in `HeadlessBoardManager`. → §3.1.
17. **`RouterSettings.clone()` drops `resultJsonPath`** and re-initializes cost arrays before
    overwriting them. → §4.
18. **`validate()` NPEs on a settings object with null `maxPasses`/`tracePullTightAccuracy`.** → §4.
19. **The board hash reported everywhere is MD5 of a Java-serialized "basic profile"**
    (traces + vias + itemList). Any Rust equivalent needs its own stable hash and must not claim
    compatibility. → §13.
20. **`strict_drc` costs a whole-board serialize per connection attempt** and rolls back by
    deserializing. → §2.1 #4, §13.
21. **Incompletes filtering differs between v19 and v2**, so published v19 benchmark scores are not
    comparable. → §15.1.
22. **Bends are counted as `cornerCount - 2` per trace** regardless of whether the corner actually
    turns; the score's bend term therefore penalizes collinear corner counts too. → §8.
23. **`ClearanceMatrix.getValue` is read as `row[classJ].column[classI]`** — asymmetric indexing on
    a symmetric-by-convention matrix. → §14.2.
24. **`clearance_safety_margin = 16` board units** is added on demand (`addSafetyMargin` flag), not
    baked into the matrix. Callers differ on whether they pass it. → §14.2.
25. **`Limits.CRIT_INT = 2^25` is a hard invariant on coordinates**, not a suggestion. → §16.
26. **`ItemSelectionFilter` categories** (`TRACES`, `VIAS`, `PINS`, `CONDUCTION`, `KEEPOUT`,
    `VIA_KEEPOUT`, `COMPONENT_KEEPOUT`, `FIXED`, `UNFIXED`, …) gate `pickItems`, which
    `splitTraces` depends on. → §11.3.
27. **The output format is chosen from the *input* format** and `setJobOutput` is wired as a
    board-update listener, so SES serialization runs repeatedly during routing. → §6.
28. **Timeout semantics**: `job_timeout` clamps to 24 h, `requestStop()` then a 30 s grace, and a
    `TIMED_OUT` job with a written output still exits `0`. → §3.1, §6.
29. **`normalizeSuppressedNetNos` is per-board-instance and transient** — a deserialized board
    candidate (BoardHistory / strictDrc rollback) forgets the suppression. → §11.6.
30. **`PolylineTrace.combine` requires identical `fixedState`**, so a SHOVE_FIXED pin-exit stub is
    never merged into the UNFIXED trace it feeds. That is deliberate and load-bearing for pin
    exits. → §11.5.

---

## 19. Rust design implications

### 19.1 Keep semantically (behavior-defining; do not "improve")

* **Item ordering.** Make item ids `u32`, dense, monotonic, and define the canonical iteration
  order as *descending id* wherever the Java code walks `UndoableObjects`. If you switch to
  ascending, expect different (not necessarily worse) routing results — but then you cannot
  A/B against upstream at all.
* **The obstacle predicate matrix (§9.3) verbatim.** Encode it as a `match` on a
  `ItemKind`×`ItemKind` pair, not as trait objects — it is a small, closed relation.
* **Fixed-state ordering and all four predicates** (`is_shove_fixed` including the net-class
  override, `is_user_fixed`, `is_deletion_forbidden` including the plane-on-non-signal-layer rule,
  `is_routable`).
* **All insert/split/merge guards** listed in §18 items 3–6. They are bug fixes purchased with
  production incidents.
* **The cost constants** (§3.5, §3.6, §5). Put them in one `const` module with the same names, so
  a diff against `DefaultSettings.java` is mechanical.
* **`clearance_compensation_value = (self_clearance + 1) / 2`** and
  `clearance_safety_margin = 16` — search-tree correctness depends on both.
* **Score formula inputs** (§8) exactly, including `bends = corner_count - 2`, the SHOVE_FIXED
  half-weighting in `totalWeightedLength`, and the mm normalization.
* **The incompletes filter** (§15.1) — it is what v2 measures.
* **Settings merge semantics**: nullable-everywhere + "non-null wins by ascending priority". This
  maps perfectly onto `Option<T>` and a `merge(&mut self, other: &Self)` derive.

### 19.2 Restructure

**Board storage → arena + generational index.**
```rust
pub struct ItemId(u32);                        // dense, monotonic, matches Java ids
pub struct Board {
    items:     Vec<Option<Item>>,              // arena indexed by ItemId
    free_list: Vec<ItemId>,
    order:     Vec<ItemId>,                    // maintained descending, or a BTreeSet<Reverse<u32>>
    // hot, split out of Item for cache locality:
    kind:      Vec<ItemKind>,                  // 1 byte
    layer:     Vec<i16>,
    nets:      NetTable,                       // CSR: net_offsets: Vec<u32>, net_ids: Vec<NetId>
    clearance_class: Vec<u16>,
    fixed:     Vec<FixedState>,                // 1 byte
    on_board:  BitVec,
}
```
Rationale: `Item` in Java is 10 fields + two transient side tables, and the hot loops
(`clearanceViolations`, `getNormalContacts`, `splitTraces`, statistics) touch only
`kind/layer/nets/clearance_class/fixed`. A struct-of-arrays layout turns those scans into
sequential reads. Geometry (`Polyline`, `Area`) stays in a side arena keyed by `ItemId`.

**Net numbers.** `int[] netNumbers` is 1–2 entries in ~all cases; use `SmallVec<[NetId; 2]>` or a
CSR side table. `shares_net` becomes a 2×2 loop with no allocation (Java allocates nothing here
either, but every `TreeSet<Item>` result does).

**Contacts and connected sets.** `getNormalContacts()` returns a fresh `TreeSet<Item>` on *every*
call and is called in nested loops (`isCycle`, `getConnectionItems`, `NetIncompletes`,
`combineAtStart`, `reduceNetsOfRouteItems`). In Rust:
```rust
fn normal_contacts_into(&self, id: ItemId, at: Point, out: &mut SmallVec<[ItemId; 8]>)
```
with a caller-owned scratch buffer, plus a per-pass memo keyed by `(item, revision)`.
`getConnectedSet` should be a **union-find** maintained incrementally across the pass rather than a
DFS re-run per query — `NetIncompletes` alone runs it once per unvisited item per net.

**`NetIncompletes` Kruskal.** Replace `joinConnectedSets` (O(n) pointer rewrite per merge, O(n²)
per net) with `union-find` (α(n)). Keep the Delaunay triangulation (it is the *definition* of the
candidate edge set — a full MST over all pairs would produce different airlines and hence a
different `maximumCount`).

**Undo.** The Java model (per-object clone chains + level stack) is over-general for a router. Use
a **command log**:
```rust
enum BoardOp { Insert(ItemId), Remove(ItemId), Replace{ id: ItemId, old_geom: GeomId } }
struct Snapshot { op_len: usize, geom_len: usize }
```
Undo = replay the log backwards. This gives O(changes) instead of O(items) per undo and removes
`saveForUndo`'s deep clone entirely. Keep `generate_snapshot` / `pop_snapshot` / `undo` / `redo`
as the public surface so callers (shove, pull-tight, optimizer) port unchanged.

**Full-board snapshots.** Replace Java serialization with a `Board::clone()` that memcpy's the
arenas (all POD after the restructure) — this is the single biggest win available, because
`strict_drc`, `BoardHistory` and `deepCopy` all round-trip through `ObjectOutputStream` today.

**Clearance matrix.** A flat `Vec<u32>` of `class_count² × layer_count` with the *same* index order
(`(class_j * class_count + class_i) * layers + layer`), plus a precomputed `max_value_on_layer`.
Add a per-`(class,layer)` compensation LUT so `clearance_compensation_value` is a load, not a
division.

**Settings.** Derive the merge: `#[derive(Merge)]` over `Option<T>` fields, with sources as a
sorted `Vec<Box<dyn SettingsSource>>`. Do **not** reproduce the reflection-based
`copyFields` primitive-default suppression — with `Option<T>` everywhere the distinction is free.
Resolve into a **non-optional `ResolvedRouterSettings`** once, before routing, so the engine never
sees `Option`. That also eliminates the ~40 null-checks-with-differing-defaults scattered through
`RouterSettings`'s accessors (§18 item 15).

**Parallelism.** The pass loop is single-threaded by design (see batch-orchestration.md §9); the
only parallel stage is the optimizer, which clones boards. With memcpy-cloneable boards, the
`GLOBAL_OPTIMAL`/`HYBRID` strategies become genuinely cheap, and a `rayon` scoped pool over
per-item candidate boards is the natural mapping. Everything in this document's scope is
single-threaded and should stay `&mut Board`.

**Geometry.** Keep `IntPoint`/`IntBox`/`IntOctagon`/`Simplex` as `i32`-based `Copy` types.
For `Line::intersection`, replace `BigInteger` with `i128` — `det1`, `det2`, `det` are all products
of coordinates bounded by 2^25, so `det1·delta2.x` fits in ~2^76 and `i128` is exact with room to
spare. This removes the entire `RationalPoint`/`RationalVector`/`BigIntDirection` family from the
hot path; keep a `Rational { x: i128, y: i128, z: i128 }` fallback only for the (rare) case where
the quotient is not integral. Expect this alone to matter on any-angle boards.

### 19.3 Java-isms observed (file:line evidence)

* **Eager log-string construction on the hottest path.**
  `rules/ClearanceMatrix.java:166-198` builds a ~15-fragment concatenated `String`
  (including two `double` divisions and two array look-ups) on **every clearance query**, then
  passes it to `FRLogger.trace`, which only *then* checks `granularTraceEnabled`
  (`logger/FRLogger.java:410-435`) — and unconditionally calls
  `DebugControl.getInstance().check(operation, impactedItems)`. v19's `get_value`
  (`v19:ClearanceMatrix.java:127-141`) is 4 lines with no allocation. Same pattern in
  `drc/NetIncompletes.java:63-133, 143-152, 205-220`, `board/trace/PolylineTrace.java:522-544,
  559-579, 621-643`, `board/facade/BoardItemRepository.java:139-150, 174-188`,
  `datastructures/UndoableObjects.java:91-105`, `autoroute/pipeline/AutorouteConnectionRouter.java:96-101,
  113-120`. **In Rust: `tracing::trace!` with lazy field capture, or a compile-time feature gate.**
* **Hard-coded net-number debug branches left in production code.**
  `PolylineTrace.java:202-203, 522-523, 559, 621-624`, `Trace.java:273-274` all test
  `netNumbers[0] == 49` (and `== 94`) and take a logging branch. Cost is small; the smell is that
  these are per-call branches in `combine`/`split`.
* **`TreeSet<Item>` allocation per query.** `Item.getAllContacts()` (`:499-519`),
  `getAllContacts(layer)` (`:525-548`), `Trace.getNormalContacts()` (`:155-166`),
  `getNormalContacts(point, ignoreNet)` (`:173-203`), `Item.getConnectedSet` (`:606-614`),
  `getConnectionItems` (`:702-781`) each allocate one or more `TreeSet`s; `getNormalContacts` is
  called ~4× per `combine` attempt and once per contact in `isCycleRecu`.
* **`LinkedList` everywhere in DRC.** `Item.clearanceViolations()` allocates a `LinkedList` plus a
  `Collection<TreeEntry>` per tile shape (`:368, 375`), and allocates two enlarged `TileShape`s per
  candidate pair (`:442-445`) plus 32 more inside the 16-step binary search (`:482-491`).
* **Full board serialization used as a value copy.**
  `BasicBoard.clone()` = `BoardSnapshotManager.serialize(false)` →
  `SafeObjectInputStream.readObject()` (`BoardSnapshotManager.java:26-55`); used by
  `RoutingBoard.deepCopy()`, `strict_drc` (`AutorouteConnectionRouter.java:84-85, 252`) and the MD5
  board hash (`:58-72`). Also `RouterSettings` is `Serializable`+`Cloneable` with a hand-written
  966-line deep copy.
* **Reflection in the settings path.** `SettingsMerger.merge` → `RouterSettings.applyNewValuesFrom`
  → `ReflectionUtil.copyFields` (`util/ReflectionUtil.java:215-330`) walks
  `getDeclaredFields()` with `setAccessible(true)` and does `Class.getDeclaredConstructor()
  .newInstance()` for nested object arrays. Fine at startup, but it also fires on every
  API-triggered settings update.
* **BigInteger churn.** `Line.intersection` (`geometry/planar/Line.java:277-303`): 8
  `BigInteger.valueOf`/`multiply`/`subtract` + 2 `mod` + 2 `divide` per general-position
  intersection, with `RationalPoint` viral downstream (125 `BigInteger` references across
  `geometry/planar` + `datastructures/BigIntAux.java`).
* **`getTraceCosts()` allocates a fresh `ExpansionCostFactor[]`** per call
  (`RouterSettings.java:877-889`), and `AutorouteControl`'s constructor allocates
  `traceHalfWidth`, `compensatedTraceHalfWidth`, `layerActive`, `viaRadii`, `addViaCosts`
  (each a `ViaCost` with its own `int[layerCount]`), and `bendCosts` — **per net, per pass**
  (`maze/AutorouteControl.java:137-150`).
* **O(n²) rescans.** `BasicBoard.combineTraces` restarts the whole `itemList` scan after every
  successful combine (`:686-704`); `normalizeTraces` re-collects the net's traces every outer
  iteration (`:752-792`); `NetIncompletes.joinConnectedSets` rewrites every `NetItem` per merged
  edge; `reduceNetsOfRouteItems` restarts the full board scan on every change (`:1287-1290`).
* **`String.formatted` in loops.** `RouterSettings.applyBoardSpecificOptimizations` builds a
  change-summary `StringBuilder` with up to five `String.formatted` calls per layer even when
  nothing changed (`:389-434`).
* **Virtual thread for post-load stats** (`HeadlessBoardManager.java:765-783`) computes a full
  `BoardStatistics` (which itself walks the board three times) concurrently with the router
  starting — a data race in spirit if not in the letter of the code, and a reason board load
  timings are noisy.

### 19.4 Suggested Rust module cut for this document's scope

```
fast_routing::board        # arena, ItemId, ItemKind, FixedState, obstacle matrix   (§9, §11, §12)
fast_routing::board::edit  # insert/remove/split/combine/normalize + guards         (§11)
fast_routing::board::undo  # command-log snapshots                                  (§13)
fast_routing::connect      # contacts, connected sets (union-find), connections,
                           # cycles, tails, net reduction                           (§10)
fast_routing::rules        # ClearanceMatrix (flat), NetClass, ViaRule, BoardRules   (§14)
fast_routing::drc          # clearance violations, incompletes (Delaunay+union-find) (§15)
fast_routing::score        # BoardStatistics + score formula                         (§8)
fast_routing::settings     # Router/Fanout/Optimizer/Scoring/Layer + sources + merge (§3–§5)
fast_routing::job          # RoutingJob, states, stop protocol, manifest             (§6, §7)
fast_routing::geom         # i128 exact kernel, Limits, TileShape family             (§16)
fast_routing::io           # DSN/SES/KiCad-JSON/rules                                (§17)
```
