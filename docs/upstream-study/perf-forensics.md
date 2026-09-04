# Freerouting Performance Forensics — where the Java burns time and memory

**Scope of this document.** This is not an algorithm spec. It is the *cost model* of the reference
implementation: an allocation- and cycle-level audit of the Java hot paths, ranked by how much a Rust
rewrite can win by doing them differently. The algorithms themselves are specified in the sibling
documents; this one deliberately does **not** re-derive them:

| Sibling spec | Covers | This doc cross-references it as |
|---|---|---|
| `maze-engine.md` | A\* room/door expansion, `MazeListElement` lifecycle, drill pages, ripup, `DestinationDistance` | **[MAZE]** |
| `search-tree-obstacles.md` | `ShapeSearchTree` / `MinAreaTree` structure, `complete_shape`, clearance compensation, per-item tile shapes | **[TREE]** |
| `shove.md` | Push-and-shove, `ShapeTraceEntries`, forced pad/via | **[SHOVE]** |
| `pull-tight-insert.md` | Connection location + insertion, `PullTightAlgo` | **[PULL]** |
| `batch-orchestration.md` | Pass scheduling, stop conditions, scoring, MT optimizer *semantics* | **[BATCH]** |

Where those documents describe *what* a routine does, this one describes *what it costs and why*.

---

## 0. Measurement baseline and method

### 0.1 The numbers we are explaining

Measured on the Java build under study (v2.3.0 lineage, `/tmp/fr-java/src/main/java/app/freerouting`):

| Workload | Wall time | Bytes allocated | Peak heap |
|---|---|---|---|
| 24-net board | ~2 s | **5.9 GB** | ~1 GB |
| 418-net keyboard | 314 s | **~3 TB** | ~1 GB |
| 46-fixture suite | — | — | **14 boards time out at 300 s** |

3 TB / 314 s ≈ **10 GB/s of allocation**. That is roughly the *memory bandwidth ceiling* of a single
core doing nothing but `new`. A modern JVM TLAB bump-allocates at that rate, so the router is
effectively spending a large fraction of its cycles in the allocator and in the young-gen collector
tracing/copying survivors. **The primary optimization target is not the algorithm; it is the allocation
rate.** Section 2 reconciles the 3 TB figure against specific call sites.

### 0.2 Two trees, one instrumented lineage

Both trees under study carry the *same* embedded diagnostic instrumentation (see §4.2): hardcoded
debug anchors on net numbers (`AutorouteEngine.java:164` `ctrl.netNumber == 33 || 66 || 67`;
`ShapeSearchTree45Degree.java:549` `netNumber == 77 && startShape.leftX == 1762393 && …`) and on a
pin-name prefix (`MazeFanoutDiagnostics.java:17` `ctrl.fanoutStartPinName.startsWith("U27-")`). These
are debugging artifacts, not upstream design. **They are present in the build that produced the
measurements above**, so they are load-bearing for interpreting those numbers — but a Rust port must
not reproduce them, and an engineer comparing against a pristine upstream jar should expect the
instrumentation tax (§4.2, ranked #2) to be smaller there while every *structural* finding (#1, #3–#10)
remains.

Where v1.9 (`src_v19`) and v2 genuinely differ in cost, this is flagged as a **v19→v2 regression** or
**improvement**, with both file:line citations. 2.3.0 executes the v2 code; v19 is cited only as
evidence about intent and as a source of already-known-good patterns.

### 0.3 Method

Read for allocation, not for logic:
1. every `new` on a path reachable from `MazeSearchEngine.occupyNextElement()`;
2. every uncached derived-geometry call (`getShape()`, `intersection()`, `corner()`, `boundingBox()`);
3. every collection constructed inside a loop;
4. every string concatenation not behind a level check;
5. every full-board scan (`board.getItems()`, `itemList.startReadObject()`) reachable per-connection;
6. `BigInteger` reachability from the maze loop (§4.8).

---

## 1. What upstream itself measures

Two mechanisms exist. Neither is on by default, and neither instruments the actual hot loops.

### 1.1 `autoroute/PerformanceProfiler.java` (168 lines, v2; 172 lines, v19 — functionally identical)

A static, thread-safe, nested-section timer.

```
static Map<String, AtomicLong> timings   = ConcurrentHashMap   // section -> total ns
static Map<String, AtomicLong> counts    = ConcurrentHashMap   // section -> invocation count
static ThreadLocal<Deque<Invocation>> stack = ArrayDeque       // per-thread nesting stack
static List<PassInfo> passHistory        = synchronizedList(ArrayList)
static ConfigInfo configInfo             // viaCosts, planeViaCosts, preferred[], against[]

start(section): stack.get().push(new Invocation(section, System.nanoTime()))
end(section):   inv = stack.pop(); if inv.name.equals(section):
                    timings[section] += nanoTime() - inv.startTime; counts[section]++
                // on mismatch: SILENTLY DROP the sample (PerformanceProfiler.java:41-56)
printResults(): sort sections by total ns desc; print "%-40s: %8d ms total, %8d calls, %6d ms avg"
                then the pass history: "Pass %-3d: %4d unrouted items, %8.2f s, %5d ripup cost"
reset():        clears timings + counts (NOT passHistory, NOT configInfo — leak across runs)
```

Two flaws worth not copying: (a) `end()` on a mismatched name silently discards the sample, and the
JavaDoc-comment block at `:44-56` is literally a stream-of-consciousness note by the author admitting
they gave up on getting the nesting right; (b) `avgMs = totalMs / count` is integer division on
milliseconds, so any section averaging < 1 ms reports **0** — which is every section in the maze loop.

**All instrumented sections, exhaustively** (grep of `PerformanceProfiler.` outside its own file):

| Section | Site (v2) | Site (v19) |
|---|---|---|
| `"board.deepCopy"` | `AutoroutePassRunner.java:54,56` | `BatchAutorouter.java:215,217` |
| `"autoroute_item"` | `AutoroutePassRunner.java:238,246` | `BatchAutorouter.java:423,426` |
| `"stats_update"` | `BatchAutorouterThread.java:405,417` | — |
| `recordPass(...)` | `AutoroutePassRunner.java:325` | `BatchAutorouter.java:541` |
| `recordConfiguration(...)` | `AutorouteBatchLoop.java:76` | `BatchAutorouter.java:186` |
| `printResults()/reset()` | `AutorouteBatchLoop.java:568,569` | `BatchAutorouter.java:317,318` |

Note what is **not** measured: room completion, the search tree, `getSectionSegments`, shove, pull-tight.
The profiler can tell you "autoroute_item took 300 s"; it cannot tell you why.

### 1.2 `autoroute/pipeline/AutorouteRuntimeMetrics.java` (66 lines, v2-only)

Thin wrappers over JMX. Every method is defensive (`try { … } catch (Throwable) { return -1f; }`):

```
cpuSecondsSnapshot(job)        -> job.resourceUsage.cpuTimeUsed
allocatedMemoryMbSnapshot(job) -> job.resourceUsage.maxMemoryUsed
peakHeapMbSnapshot(job)        -> job.resourceUsage.peakMemoryUsed
currentThreadCpuSeconds()      -> com.sun.management.ThreadMXBean.getThreadCpuTime(tid) / 1e9
currentThreadAllocatedMb()     -> setThreadAllocatedMemoryEnabled(true);
                                  getThreadAllocatedBytes(tid) / 1MiB      <-- THE 5.9 GB / 3 TB NUMBER
currentHeapUsageMb()           -> MemoryMXBean.getHeapMemoryUsage().getUsed() / 1MiB
nanosToMillis(nanos)           -> nanos / 1e6
```

Callers: only `AutorouteBatchLoop.java:93-195` (wrapping the **fanout stage only**) and
`BatchAutorouter.java:221-237` (formatting). So upstream's own allocation telemetry covers fanout and
nothing else — the 3 TB figure comes from `getThreadAllocatedBytes`, which is exact and is
*per-thread*: it includes TLAB waste but is the JVM's own count of bytes handed out.

Note `currentThreadAllocatedMb()` calls `setThreadAllocatedMemoryEnabled(true)` on **every**
invocation (`:46`) — a JMX round-trip inside what is nominally a cheap probe.

### 1.3 The `freerouting.benchmark.*` system properties (v2-only)

```java
// BatchAutorouter.java:61-64
private static final boolean BENCHMARK_PROFILE_ENABLED =
    Boolean.getBoolean("freerouting.benchmark.profile");
private static final boolean BENCHMARK_RETAIN_AUTOROUTE_DATABASE =
    Boolean.getBoolean("freerouting.benchmark.retain_autoroute_database");
```

`-Dfreerouting.benchmark.profile=true` enables a set of `System.nanoTime()` accumulators on
`BatchAutorouter` (`:96-105`), summarized in `logBenchmarkProfile` (`:217-237`):

| Counter | Accumulated at | What it covers |
|---|---|---|
| `profileItemSelectionNanos` | `AutoroutePassRunner.java:160` | `getAutorouteItems` (see §4.11) |
| `profileBoardStatisticsNanos` | `AutoroutePassRunner.java:174,311,503` | `new BoardStatistics(board,…)` |
| `profileIntermediateStatisticsNanos` | `AutoroutePassRunner.java:192,312` | DRC incompletes |
| `profileAutorouteItemNanos` | `AutoroutePassRunner.java:248` | one connection end-to-end |
| `profileMazeSearchNanos` | `AutorouteConnectionRouter.java` via `addProfileMazeSearchNanos` | `autorouteConnection` |
| `profileOptChangedAreaNanos` | via `addProfileOptChangedAreaNanos` | pull-tight **[PULL]** |
| `profileTailRemovalNanos` | `BatchAutorouter.java:500` | `removeTails` |
| `profileIncompleteDrcNanos` | `BatchAutorouter.java:557-562` | `calculateIncompleteCount` |
| `profileRouteItemCount` / `profilePlaneItemCount` | `AutoroutePassRunner.java:230,233` | counts |

`-Dfreerouting.benchmark.retain_autoroute_database=true` is the single most important flag in the
codebase and is discussed as attack surface **#3** (§4.4).

### 1.4 Comments in which upstream admits a bottleneck

Verbatim, with citations — these are the author's own confessions and each maps to an attack surface:

- `BatchAutorouter.java:151-153`: *"Retained state is deliberately opt-in for bounded benchmark
  experiments only. The default remains the fresh-engine behavior used by production routing and
  parity comparisons."* → **#3**. Upstream knows the database can be retained and chose parity over speed.
- `BatchAutorouter.java:55-57`: *"Progress statistics are informational only; avoid rebuilding the
  expensive snapshot for every item while keeping the GUI reasonably current on large boards."*
  (guarding `PROGRESS_STATISTICS_ITEM_INTERVAL = 10`) → **#10**.
- `BatchAutorouter.java:70-71`: *"Reusable collections to reduce memory churn (thread-safe as each
  thread has its own BatchAutorouter instance)"* — a one-off fix applied to exactly two fields
  (`reusableAutorouteItemList`, `reusableHandledItems`) and nowhere else.
- `BatchOptimizerMultiThreaded.java:249-250`: *"We schedule just enough tasks to keep workers busy in
  order not to exhaust JVM memory so that it can run on systems without huge amount of RAM using the
  pool"* — **the code immediately below the comment does the opposite**: `for (int t = 0; t <
  itemIds.size(); t++) { … new OptimizeRouteTask(…); pool.execute(newTask); }` (`:251-260`), and
  `OptimizeRouteTask`'s *constructor* performs a full board deep copy (`OptimizeRouteTask.java:30`).
  → **#4**.
- `DrillPage`'s existence is itself a documented performance workaround —
  `MazeSearchAlgo.java:1003-1006` (v19): *"A drill page is inserted between an expansion room and the
  drill to expand in order to prevent performance problems with rooms with big shapes containing many
  drills."* → §4.5.
- `SortedRoomNeighbours.java:340`: *"insert only overlap_doors between items of the same net **for
  performance reasons**"* — a correctness-affecting pruning motivated by cost.
- `ShapeSearchTree.java:418-421`: the redundant-intersection skip for `IntOctagon`-vs-`IntOctagon`
  is *"skipped for performance reasons"*.
- `Line.java:80-82` (`fastEquals`), `:122` (`sideOf(FloatPoint,…)` — *"only implemented for IntPoint
  lines for performance reasons"*), `:422-423` (`compareTo` — *"Fast implementation only for lines
  consisting of IntPoints because of critical performance"*), `IntVector.java:20` (*"range check
  omitted for performance reasons"*): the geometry layer is riddled with hand-specialized fast paths,
  which is direct evidence that `geometry.planar` is a measured hot spot.
- `Line.java:154-161` (`sideOfIntersection`): *"Previous calculation was with FloatPoints and a
  tolerance for performance reasons. Make an exact check for collinearity now with class Point"* —
  this is the exact door through which `BigInteger` enters the hot path (§4.8).

---

## 2. The allocation budget — reconciling 3 TB

A single spatial query is the atomic unit of cost. Here is its allocation bill in v2, following
`board.overlappingObjects(shape, layer)` down:

```
ShapeSearchTree.overlappingObjects(shape, layer)              [ShapeSearchTree.java:370-373]
  new TreeSet<SearchTreeObject>()                                     ~48 B + 40 B/entry
  new int[0]                                                          ~16 B
  -> overlappingObjects(shape, layer, ignoreNets, result)     [:355-358]
       new LinkedList<TreeEntry>()                                    ~48 B + 40 B/node
       -> overlappingTreeEntries(shape, layer, ignoreNets, out) [:390-435]
            shape.boundingShape(boundingDirections)                   1 IntOctagon (~64 B)
            -> MinAreaTree.overlaps(bounds)                    [MinAreaTree.java:25-48]
                 new TreeSet<Leaf>()                                  ~48 B + 40 B/entry
                 new ArrayStack<TreeNode>(10000)                      *** new Object[10000] = 80 016 B ***
            per surviving leaf: new TreeEntry(object, idx)            ~24 B
```

**Floor: ~80 KB per spatial query, dominated by one array that is 99.9 % untouched.**

Reconciliation: 3 TB / 80 KB ≈ **37 M spatial queries** in 314 s ≈ 120 k queries/s. That is entirely
plausible: each maze expansion of a room triggers `completeNeighbourRooms` → `completeExpansionRoom` →
`ShapeSearchTree.completeShape` (which allocates its *own* 80 KB stack at
`ShapeSearchTree.java:609` / `ShapeSearchTree45Degree.java:148`), and every connectivity walk
(`getNormalContacts`) issues two more (§4.11). A single 418-net board at ~50 passes × ~800 items ×
~20 connections' worth of room completions and connectivity walks reaches tens of millions trivially.

For the small board: 5.9 GB / 80 KB ≈ 74 k queries in 2 s — again plausible, and it means **even the
2-second case is ~100 % allocator-bound, not algorithm-bound**.

The remaining budget (roughly: whatever the 80 KB arrays do not explain) is split between the
instrumentation strings (§4.2), uncached `TileShape` intersections (§4.3, §4.6), and the per-expansion
object tuples (§4.7).

---

## 3. Top 10 attack surfaces, ranked

Impact estimates are for a Rust rewrite that keeps the *same algorithm* and only changes the
representation/lifetime strategy. They are multiplicative-ish but overlapping; treat them as
"share of the Java cost this eliminates".

| # | Attack surface | Evidence | Est. impact | Rust fix |
|---|---|---|---|---|
| 1 | **80 KB traversal stack allocated per spatial query** | `MinAreaTree.java:30`, `ShapeSearchTree.java:609`, `ShapeSearchTree45Degree.java:148`, `ShapeSearchTree90Degree.java:69` | **30–50 % of all bytes.** v19 had this right (`MinAreaTree.java:19` field + `:32` reset); v2 regressed it | `SmallVec<[NodeIdx; 64]>` on the stack, or a per-thread scratch arena reset per query. Zero heap traffic. |
| 2 | **Unguarded `FRLogger.trace(...)` with eager string + geometry in the innermost maze loops** | `MazeSearchEngine.java:421-438, 559-588, 800-848, 907-963`; `ShapeSearchTree.java:651-666`; `MazeExpansionEngine.java:40-49, 102-111` | **15–35 % of time and bytes** in the measured build; ~0 in a clean upstream but *the shape of the mistake* must be avoided | `tracing` with `event!` macros (args lazily evaluated) or compile-time `#[cfg(feature="trace")]`. Never call `getShape()` to build a log line. |
| 3 | **The entire autoroute database is destroyed and rebuilt per connection** | `BatchAutorouter.java:154` (`retainAutorouteDatabase = false` by default), `RoutingBoard.java:887-894`, `AutorouteEngine.java:201-205, 307-317` | **2–10× on dense boards.** Rebuilds every expansion room, every drill page, every drill convex decomposition | Persist rooms/doors/drill pages across connections; invalidate incrementally by dirty rectangle. §4.4. |
| 4 | **Board deep copy = Java serialization round-trip, once per optimizer *task*** | `RoutingBoardUndoFacade.java:41-72`, `OptimizeRouteTask.java:30`, `BatchOptimizerMultiThreaded.java:251-260`, `BasicBoard.java:1388-1400` | **The ~1 GB peak heap, and most of optimizer wall time** | Structural sharing (`Arc` + copy-on-write items) or an arena snapshot (`memcpy` of a `Vec<Item>` + index rebuild). §4.10. |
| 5 | **`ExpansionDoor.getShape()` recomputes a convex intersection on every call, uncached** | `ExpansionDoor.java:43-47`; called from `getSectionSegments` `:107`, `doorIsSmall` `:767`, and 6× per door from the trace helpers | **10–20 % of maze time** | Compute the door shape once at door construction; store it inline in the door. §4.3. |
| 6 | **Connectivity is recomputed geometrically instead of cached** | `Trace.java:155-201` (2 spatial queries per call), `Item.java:596-635, 676-690`, `BatchAutorouter.java:345-380` | **Quadratic pass-startup; 20 %+ on large boards** | Persistent net adjacency graph + union-find, updated on insert/remove. §4.11. |
| 7 | **`TileShape`/`Simplex` intersection allocates 3 arrays + N `Line` objects and re-sorts every time** | `Simplex.java:620-630`, `:884-1010` (`removeRedundantLines`) | **10–15 %** | Fixed-capacity half-plane buffers (`ArrayVec<HalfPlane, 16>`), no re-sort when both inputs are already sorted, in-place redundancy elimination. §4.8. |
| 8 | **Per-expansion object tuple: `MazeListElement` + `FloatLine` + 3 `FloatPoint` + `IntBox` + 2 `IntPoint`** | `MazeSearchEngine.java:853-901`, `DestinationDistance.java:117-119`, `FloatPoint.java:334-338` | **5–10 %; plus the `TreeSet` node per queue insert** | `MazeListElement` as a `Copy` struct in a `Vec`-backed binary heap; `FloatLine`/`FloatPoint` as `Copy` values; compute the heuristic without materializing an `IntBox`. §4.7. |
| 9 | **`TreeSet`/`LinkedList` everywhere, including as the A\* frontier** | `MazeSearchEngine.java:84-125` (`SortedSet<MazeListElement>` = `TreeSet`), `:559` `new LinkedList<>(room.getDoors())` per expansion, `SortedRoomNeighbours.java:54,55,200,204-213` | **5–10 %; plus terrible locality** | `BinaryHeap` / bucketed queue; `SmallVec` for door lists; sort a slice instead of a `LinkedList`. §4.9. |
| 10 | **Full-board statistics/ratsnest recomputation inside the pass loop** | `AutoroutePassRunner.java:174, 180-192, 300-312, 497-505`, `BatchAutorouter.java:556-564`, `DesignRulesChecker.java:542-556` | **3–8 %, more on huge boards** | Incremental incompleteness counters maintained by the union-find in #6; snapshot only on demand. §4.12. |

Honourable mentions that did **not** make the top 10 but are free wins:
`IntPoint`'s constructor performs two `Math.abs` range checks per allocation and calls
`FRLogger.debug` on violation (`IntPoint.java:19-29`) — in Rust this is a `debug_assert!`;
`ItemSearchTreesInfo.getPrecalculatedTreeShapes` is a **linear scan of a `LinkedList` with identity
comparison** on every `getTreeShape` call (`ItemSearchTreesInfo.java:51-58`) — in Rust the tree index
is a `u8` array slot;
`AutorouteControl`'s constructor allocates 7 arrays plus `layerCount` × `int[layerCount]` per
connection (`AutorouteControl.java:139-186`) — hoist it out of the loop.

---

## 4. Detailed findings

### 4.1 Attack surface #1 — the 80 KB-per-query traversal stack

`ArrayStack<T>` (`datastructures/ArrayStack.java`) is a trivial array stack:

```java
public ArrayStack(int maxStackDepth) { nodeArr = (T[]) new Object[maxStackDepth]; }   // :14-16
public void reset() { level = -1; }                                                    // :19-21
private void reallocate() { T[] newArray = (T[]) new Object[4 * this.nodeArr.length]; … } // :45-49
```

Initial capacity is always **10 000**, growth factor **4×**. On a 64-bit JVM with compressed oops
that is a 40 016-byte array; without, 80 016 bytes. Real R-tree traversal depth for a board with
~10⁴ leaves is ~30–60 entries; the array is ~99.5 % untouched, but the JVM must still zero it
(`new Object[n]` is zero-initialized) — so this is not merely wasted bytes, it is **80 KB of `memset`
per query**, which is where a good chunk of the "10 GB/s" actually goes.

**v19 got this right and v2 regressed it.** In v19 the stack is a reusable field on the tree:

```java
// src_v19/.../datastructures/MinAreaTree.java:19
protected ArrayStack<TreeNode> node_stack = new ArrayStack<>(10000);
// :32-33
this.node_stack.reset();
this.node_stack.push(this.root);
```

and the same pattern in `src_v19/.../board/ShapeSearchTree.java:607`,
`ShapeSearchTree45Degree.java:125`, `ShapeSearchTree90Degree.java:67`.

v2 moved to per-call allocation at **all four** sites:

| v2 site | Method | v19 equivalent |
|---|---|---|
| `datastructures/MinAreaTree.java:30` | `overlaps(RegularTileShape)` | `MinAreaTree.java:32` (reused) |
| `board/searchtree/ShapeSearchTree.java:609` | `completeShape(...)` (any-angle) | `ShapeSearchTree.java:607` (reused) |
| `board/searchtree/ShapeSearchTree45Degree.java:148` | `completeShape(...)` (45° — **the default**) | `ShapeSearchTree45Degree.java:125` (reused) |
| `board/searchtree/ShapeSearchTree90Degree.java:69` | `completeShape(...)` | `ShapeSearchTree90Degree.java:67` (reused) |

The v2 change is almost certainly motivated by thread-safety for the MT optimizer (`BatchOptimizerMultiThreaded`
runs `deepCopy`'d boards on N threads, and a shared mutable field on the tree would race). The *correct*
fix is a per-thread scratch buffer, not per-call allocation.

The other allocations on the same path (`MinAreaTree.java:26` `new TreeSet<Leaf>()`;
`ShapeSearchTree.java:357` `new LinkedList<TreeEntry>()`; `:371` `new TreeSet<SearchTreeObject>()`;
`:382` `new int[0]`; `:417` `new TreeEntry(...)` per hit) add a red-black node (~40 B) per result and
a boxed entry per result. On a query returning 20 items that is ~2 KB — trivial next to the 80 KB, but
it is the *sorted-set* requirement that forces `Leaf.compareTo` (`ShapeTree.java:217-224`, which
compares `object.compareTo(other.object)` then `shapeIndexInObject`) to run O(k log k) times per query
purely to obtain determinism.

**Rust:** the query is `fn overlaps(&self, bounds: Aabb, out: &mut Vec<LeafId>)`, with the traversal
stack a `SmallVec<[NodeIdx; 64]>` local (spills to heap only on pathological depth), `out` a caller-owned
reusable `Vec` cleared by the caller, and determinism obtained by `out.sort_unstable_by_key(|l|
(l.object_id, l.shape_index))` **once** at the end rather than by an ordered container. Node indices are
`u32` into an arena, not pointers, so the traversal is a linear scan of a cache-friendly array.

### 4.2 Attack surface #2 — trace instrumentation in the innermost loops

`FRLogger.trace(String)` (`logger/FRLogger.java:372-383`) is *not* level-guarded at the call site:

```java
public static LogEntry trace(String msg) {
  if (!enabled) return null;
  if (logger == null) logger = LogManager.getLogger(Freerouting.class);
  logger.trace(msg);          // log4j2 drops it here, AFTER the string was built
  return null;
}
```

`FRLogger.isTraceEnabled()` exists (`:357-364`) but is used by **no** call site in `autoroute/`.
So the argument expression — including any geometry it triggers — is fully evaluated at every call.

The 5-argument `trace(method, operation, message, impactedItems, impactedPoints)` (`:406-434`) is
worse in one respect: `DebugControl.getInstance().check(operation, impactedItems)` runs
**outside** the `if (enabled)` block (`:427`), though `check` itself bails in ~3 field reads when
single-stepping and `traceInsertionDelay` are both off (`DebugControl.java:151-160`). The real cost is
the caller-side `toImpactedPoints(shapeEntry)` (`MazeSearchEngine.java:287-292`), which allocates a
`Point[2]` plus two rounded `IntPoint`s on **every** call.

#### The per-candidate-door describe loop — the single worst site

`MazeSearchEngine.expandToRoomDoors`, `:571-588`:

```java
for (int doorIndex = 0; doorIndex < roomDoorsSnapshot.size(); doorIndex++) {
  ExpansionDoor candidateDoor = roomDoorsSnapshot.get(doorIndex);
  FRLogger.trace("ROOM_DOOR candidate index=" + doorIndex
      + ", from_section=" + listElement.sectionNoOfDoor
      + ", backtrack_section=" + listElement.sectionNoOfBacktrackDoor
      + ", from_door=" + describeExpandable(listElement.door)      // <-- getShape() -> intersection
      + ", nextRoom=" + describeRoom(listElement.nextRoom)         // <-- getShape().boundingBox()
      + ", candidate=" + describeExpandable(candidateDoor)         // <-- getShape() -> intersection
      + ", net=" + ctrl.netNumber);
}
```

`describeExpandable` (`:206-250`) ends with `IntBox bounds = door.getShape().boundingBox();` and
`describeRoom` (`:268-285`) with `room.getShape().boundingBox()`. So for a room with **D** doors this
loop performs **2D** convex-polygon intersections and **3D** bounding-box computations, builds **D**
strings of ~200 characters each — and throws all of it away. `roomDoorsSnapshot` is itself a
`new LinkedList<>(listElement.nextRoom.getDoors())` allocated one line earlier (`:559`), and there is a
second unconditional `FRLogger.trace("ROOM_DOOR context …", …)` at `:560-570` before the loop, plus the
`"ROOM_COMPLETE_SYNC"` trace at `:421-438` which also calls `describeExpandable` + `describeRoom`.

`describeExpandable` additionally calls `safeMazeSectionCount(door)` (`:252-258`) inside a
`try { … } catch (RuntimeException)` — i.e. exception-based control flow, per door, per expansion.

The **real** door loop immediately after (`:590-598`) calls `expandToDoor` → `toDoor.getSectionSegments(halfWidth)`
→ **another** `getShape()` per door. Net effect: the trace loop roughly **triples** the convex-geometry
work of the door-expansion step.

#### Full inventory of unguarded trace sites on the hot path (v2)

| File:line | Fires when | Cost per fire |
|---|---|---|
| `MazeSearchEngine.java:421-438` | every `expandToRoomDoors` | 1 door `getShape()` + 1 room `boundingBox` + string |
| `MazeSearchEngine.java:560-570` | every `expandToRoomDoors` past the ripup gate | same |
| `MazeSearchEngine.java:573-587` | **× number of doors** in the room | 2 `getShape()` + 1 `boundingBox` + string, per door |
| `MazeSearchEngine.java:800-822` | every *rejected* door section | 4 `getShape()`-backed describes |
| `MazeSearchEngine.java:823-848` | same, 5-arg variant | 4 more + `Point[2]` |
| `MazeSearchEngine.java:907-933` | every *accepted* door section | 4 describes |
| `MazeSearchEngine.java:934-963` | same, 5-arg variant | 4 more + `Point[2]` |
| `ShapeSearchTree.java:651-666` | every (obstacle × room-piece) 2-D overlap during any-angle room completion | **4** `boundingBox()` + string |
| `AutorouteEngine.java:435-448, 453-466, 477-486, 500-509, 536-543` | every room completion / candidate | `describeShapeBounds` per shape |
| `SortedRoomNeighbours.java:243-245, 448-457, 466-475, 489-500` | room-neighbour calculation, edge removal | `boundingBox()` + string |
| `Sorted45DegreeRoomNeighbours.java:343-371, 402-419` | 45° edge removal (the default mode) | `boundingBox()` + string |
| `SortedOrthogonalRoomNeighbours.java:448-570` (7 sites) | 90° mode | ditto |
| `RoutingBoard.java` (18 `FRLogger.trace` sites, e.g. `:865-874`) | trace insertion, per segment | `pickItems()` — **a full spatial query per log line** |
| `MazeExpansionEngine.java:40-49, 102-111` | every drill expansion | 3–4 `describeExpandable` — **and `enabled()` is checked only *inside* the callee** |

That last one deserves its own note. `MazeFanoutDiagnostics.trace(event, message)`:

```java
// MazeFanoutDiagnostics.java:14-18
boolean enabled() {
  return ctrl.isFanout && ctrl.fanoutStartPinName != null
      && ctrl.fanoutStartPinName.startsWith("U27-");     // hardcoded pin prefix
}
// :30-33
void trace(String event, String message) { if (!enabled()) return; … }
```

The guard is inside the method, so the caller has already built `message`. At
`MazeExpansionEngine.java:40-49` and `:102-111` that message concatenates three or four
`MazeSearchEngine.describeExpandable(...)` results. Since `enabled()` is essentially always false
(it requires a pin named `U27-*`), this is **100 % wasted work on every single drill expansion**.
Compare `:151` in the same file, which correctly does `if (search.fanoutDiagnostics.enabled()) { … }` —
the codebase is inconsistent with itself.

Guarded (cheap) counter-examples, for contrast: `ShapeSearchTree45Degree` gates its
`traceCompleteShapeFilter` / `traceCompleteShapeCandidate` / `traceCompleteShapeDecision` calls behind
`boolean debugAnchor` computed once per `completeShape` (`:144`, `:167`, `:183`, `:249`), where
`isCompleteShapeDebugAnchor` (`:547-554`) is a hardcoded coordinate match. That is the right shape —
except the anchor is a leftover hardcode for one specific board.

**Rust:** use `tracing`'s `event!(Level::TRACE, ...)` macros, whose field expressions are only evaluated
if a subscriber is interested; or gate the whole diagnostic module behind a cargo feature. Never allow
a diagnostic to call a geometry accessor. A `Display` impl that lazily formats is fine; eagerly building
`String` is not.

### 4.3 Attack surface #5 — `ExpansionDoor.getShape()` is a computation, not an accessor

```java
// autoroute/expansion/ExpansionDoor.java:41-47
@Override
public TileShape getShape() {
  TileShape firstShape  = firstRoom.getShape();
  TileShape secondShape = secondRoom.getShape();
  return firstShape.intersection(secondShape);        // NOT CACHED
}
```

Every call is a fresh convex-polygon intersection. In any-angle mode that is `Simplex.intersection`
(§4.8): allocate `Line[m+n]`, `Arrays.sort`, `new Simplex`, `removeRedundantLines()` → another
`Line[]` + a `Side[]` + an O(k²)-ish elimination loop with `sideOfIntersection` per surviving line.
In 45° mode it is the cheap `IntOctagon.intersection` (`IntOctagon.java:378-390`: 8 min/max + one
allocation + `normalize()`). Either way it is *not free*, and the result — including its lazily
computed `precalculatedCorners` / `precalculatedFloatCorners` / `precalculatedBoundingBox` caches
(`Simplex.java:22-26`) — is **discarded immediately**, so the per-shape memoization in `Simplex` never
pays off.

Call sites per single expansion of a room with D doors:

| Caller | `getShape()` calls |
|---|---|
| `doorIsSmall(currentDoor, …)` `MazeSearchEngine.java:767` | 1 (the from-door) |
| trace helpers `:421-438, :560-588` | 2D + 1 |
| `expandToDoor` → `getSectionSegments` `ExpansionDoor.java:107` | D |
| `getSectionSegments` → `calcDoorLineSegment` `:150-151` (2-D doors) | 2 more each |
| `expandToDoorSection` bend cost `MazeSearchEngine.java:859` | 1 (`backtrackDoor.getShape().centreOfGravity()`), only when `bendCosts[layer] > 0` |
| `expandToDoorSection` trace helpers `:800-848, :907-963` | 4 per section, accepted **or** rejected |
| `MazeExpansionEngine.expandToDrill` `:39` | 2 (drill shape ∩ backtrack-door shape test) |

`getSectionSegments` (`ExpansionDoor.java:105-143`) also calls `allocateSections(sectionCount)`
(`:192-201`) which reallocates `sectionArr` **and discards all occupancy state** if the count changed.
It early-returns when the length matches, and `sectionCount` derives from
`compensatedTraceHalfWidth[layer]` which is constant within a connection, so in practice it is stable
— but note the invariant: *changing the trace width mid-search silently resets door occupancy.*

Constants in `getSectionSegments` worth carrying over:
- `offset = offsetParam + AutorouteEngine.TRACE_WIDTH_TOLERANCE`, `TRACE_WIDTH_TOLERANCE = 2`
  (`AutorouteEngine.java:41`);
- 2-D free-space doors shorter than `2*offset` (i.e. `distanceSquare < 4*offset²`) yield **zero**
  sections (`:128-131`);
- `maxDoorSectionWidth = 10 * offset`; `sectionCount = floor(len / maxDoorSectionWidth) + 1` (`:138-140`).

**Rust:** the door shape is a function of two room shapes that do not change while the door exists.
Compute it in the `ExpansionDoor` constructor (which *already* computes the intersection once just to
get `dimension`, `ExpansionDoor.java:38`!) and store it inline. Store the section segments alongside,
computed once per (door, half-width) pair. This alone removes the majority of convex-intersection work
from the maze loop.

### 4.4 Attack surface #3 — the AutorouteEngine lifecycle: everything is thrown away per connection

#### The switch

```java
// pipeline/BatchAutorouter.java:151-154
// Retained state is deliberately opt-in for bounded benchmark experiments only. The default
// remains the fresh-engine behavior used by production routing and parity comparisons.
this.retainAutorouteDatabase = BENCHMARK_RETAIN_AUTOROUTE_DATABASE;   // = false unless -D flag
```

v19 hardcodes the same: `src_v19/.../BatchAutorouter.java:102` `this.retain_autoroute_database = false;`.
`BatchAutorouterThread.java:90` also hardcodes `false` for the MT path.

#### What `false` costs

```java
// board/facade/RoutingBoard.java:885-895
public AutorouteEngine initAutoroute(int netNumber, int traceClearanceClassIndex,
                                     Stoppable thread, TimeLimit timeLimit,
                                     boolean retainAutorouteDatabase) {
  if (this.autorouteEngine == null
      || !retainAutorouteDatabase                                   // <-- always true
      || this.autorouteEngine.autorouteSearchTree.compensatedClearanceClassNo != traceClearanceClassIndex) {
    this.autorouteEngine = new AutorouteEngine(this, traceClearanceClassIndex, retainAutorouteDatabase);
  }
  this.autorouteEngine.initConnection(netNumber, thread, timeLimit);
  return this.autorouteEngine;
}
```

`!retainAutorouteDatabase` short-circuits to `true`, so **a brand-new `AutorouteEngine` is constructed
for every connection attempt**, and again for the necked retry (`AutorouteConnectionRouter.java:76-83`
and `:204-211`).

Each construction (`AutorouteEngine.java:83-93`):
1. resolves the compensated search tree (cached in `SearchTreeManager`, so cheap — *unless* it is the
   first use of that clearance class, in which case `getAutorouteTree` (`SearchTreeManager.java:140-171`)
   builds a whole new R-tree by scanning `board.itemList` and inserting every item);
2. `maxDrillPageWidth = max(5 * rules.getDefaultViaDiameter(), 10000)`;
3. `new DrillPageArray(board, maxDrillPageWidth)` — allocates `rowCount × columnCount` `DrillPage`
   objects, each of which allocates `layerCount` `MazeSearchElement`s (`DrillPage.java:39-42`).

And at the end of `autorouteConnection` (`AutorouteEngine.java:201-205`):

```java
if (!this.maintainDatabase) { this.clear(); } else { this.resetAllDoors(); }
```

`clear()` (`:306-317`) removes **every** `CompleteFreeSpaceExpansionRoom` from the search tree
(`removeFromTree`, i.e. an R-tree deletion each, `MinAreaTree.remove_leaf` **[TREE]** §1.4), nulls the
room lists, and calls `board.clearAllItemTemporaryAutorouteData()` — a full linear pass over
`board.itemList` nulling every item's `ItemAutorouteInfo` (`RoutingBoard.java:1241-1250`,
`Item.java:1052-1054`). Nulling `ItemAutorouteInfo` discards, per item:
- the `ObstacleExpansionRoom[]` (one room per tree shape — for a polyline trace with 30 segments that
  is 30 rooms, each with its door list) (`ItemAutorouteInfo.java:20, 54-81`);
- the `precalculatedConnection` (`:17`), forcing `Connection.get` to re-walk the net geometrically
  (§4.11).

So per connection, the router pays: build drill page array → lazily rebuild every touched drill page's
convex decomposition → build every expansion room from scratch via `completeShape` → build every
obstacle room → then destroy all of it.

#### What `true` would have cost instead

`resetAllDoors()` (`:653-669`) — walk the complete-room list resetting door sections, walk **all board
items** resetting their obstacle-room doors and clearing `precalculatedConnection`, and
`drillPageArray.reset()` (which resets `MazeSearchElement`s but **keeps** `drills`,
`DrillPage.java:154-164`). Plus, on a net change, `initConnection` (`:96-123`) invalidates net-dependent
rooms and calls `additionalUpdateAfterChange` for every item of the new net
(`RoutingBoard.java:94-117`, which does a spatial query per item tree-shape and removes overlapping
rooms). That is still O(items) per connection, but it is *linear bookkeeping* rather than *quadratic
geometry*.

The reason upstream keeps it off is stated in the comment: **parity**. Retaining the database changes
which rooms exist when a connection starts, which changes tie-breaking, which changes routes. This is
important for the Rust port: incremental room caching is not behavior-preserving unless the invalidation
is exact.

#### Invalidation strategy, precisely (so Rust can make it exact)

`RoutingBoard.additionalUpdateAfterChange(item)` (`:94-117`), the only invalidation hook, runs **only
when `maintainDatabase` is true**:

```
for i in 0..item.treeShapeCount(autorouteSearchTree):
    shape = item.getTreeShape(tree, i)
    autorouteEngine.invalidateDrillPages(shape)                 # DrillPage.drills = null for overlapping pages
    for obj in tree.overlappingObjects(shape, item.shapeLayer(i)):
        if obj is CompleteFreeSpaceExpansionRoom:
            autorouteEngine.removeCompleteExpansionRoom(obj)
item.clearAutorouteInfo()
```

and `removeCompleteExpansionRoom` (`AutorouteEngine.java:377-412`) does the delicate part: for each door
of the removed room, detach it from the neighbour, and if `roomShape ∩ neighbourShape` is 1-dimensional,
**recreate an `IncompleteFreeSpaceExpansionRoom` on the neighbour's side** bounded by the single
half-plane `neighbourShape.borderLine(touchingSides[1]).opposite()` (`:392-400`), then invalidate the
drill pages over the removed room's shape (`:411`). This is exactly the incremental-repair algorithm a
Rust implementation should port; it is dead code in the shipped default configuration.

**Rust:** make retention the *only* mode. Keep rooms/doors in an arena keyed by `(layer, room_id)`;
maintain a dirty-rect list of board mutations; on connection start, evict rooms intersecting the dirty
rects and repair neighbours per the algorithm above. Reserve a "fresh database" mode behind a flag for
differential testing against the Java.

### 4.5 Drill pages — the most expensive cache in the system

`DrillPageArray` (`autoroute/drill/DrillPageArray.java`) tiles the board bounding box:

```
maxPageWidth  = max(5 * rules.getDefaultViaDiameter(), 10000)        # AutorouteEngine.java:89-90
columnCount   = ceil(boardWidth  / maxPageWidth)                     # :38
rowCount      = ceil(boardHeight / maxPageWidth)                     # :39
pageWidth     = ceil(boardWidth  / columnCount)                      # :40
pageHeight    = ceil(boardHeight / rowCount)                         # :41
pages[row][col] = DrillPage(IntBox(...), board)                      # :43-61
```

`overlappingPages(shape)` (`:76-97`) computes the integer index window from
`shape.boundingBox().intersection(bounds)`, then **still** performs a full `shape.intersection(page.shape)`
per candidate cell and keeps those with `dimension() > 1`. Allocates a `LinkedList` per call. Called
once per free-space room expansion with vias allowed (`MazeSearchEngine.java:607-608`).

`DrillPage.getDrills(engine, attachSmd)` (`DrillPage.java:63-131`) is the heavy part, cached by
`(this.drills != null && engine.getNetNumber() == this.netNumber)`:

```
overlaps = searchTree.overlappingTreeEntries(page.shape, layer = -1)   # ALL layers; one 80 KB query
cutoutShapes = []
prevObstacleShape = IntBox.EMPTY
for entry in overlaps:
    item = entry.object; skip if not Item
    skip if item.isDrillable(netNumber)
    skip if item is Pin and attachSmd and pin.drillAllowed()
    shape = item.getTreeShape(searchTree, entry.shapeIndexInObject)
    if not prevObstacleShape.contains(shape):        # dedup vias with identical shape on all layers
        cut = shape.intersection(page.shape)
        if cut.dimension() == 2: cutoutShapes.add(cut)
    prevObstacleShape = shape
holes = cutoutShapes.toArray()
shapeWithHoles = PolylineArea(page.shape, holes)
drillShapes = shapeWithHoles.splitToConvex(stoppableThread)           # *** FULL CONVEX DECOMPOSITION ***
for s in drillShapes:
    loc = (attachSmd ? pinCenterInDrill(s, layer0) ?? pinCenterInDrill(s, layerLast) : null)
          ?? s.centreOfGravity().round()
    drill = ExpansionDrill(s, loc, 0, layerCount-1)
    if drill.calculateExpansionRooms(engine): drills.add(drill)       # 1 spatial query + up to L room completions
```

`splitToConvex` on a polygon-with-holes is the single most expensive geometric operation in the router.
`calculateExpansionRooms` (`ExpansionDrill.java:55-92`) then issues one `overlappingObjects` query and,
for each layer with no existing room at the drill location, a full `completeExpansionRoom` — i.e. up to
`layerCount` room completions **per drill**.

Because the cache key includes `netNumber` **and** the `DrillPageArray` is recreated per connection
(§4.4), the entire decomposition is recomputed for every connection that touches a page. `invalidate()`
(`:170-172`) merely nulls `drills`; there is no partial repair.

**Rust:** keep the page grid for the whole session. Cache per page: (a) the net-independent convex
decomposition of "page minus non-drillable obstacles" (recompute only on dirty-rect intersection), and
(b) a per-net bitmask of which candidate drills are usable. Cache `centreOfGravity().round()` with the
shape. The `prevObstacleShape.contains(...)` dedup is a cheap trick that works because
`overlappingTreeEntries` returns leaves in `Leaf.compareTo` order (object id, then shape index), so a
via's per-layer identical shapes are adjacent — preserve that ordering guarantee or the dedup silently
stops working.

### 4.6 Room completion — the allocation profile of `SortedRoomNeighbours`

**[MAZE]** §3 and **[TREE]** §4 specify what this does. The cost view:

`SortedRoomNeighbours.calculateNeighbours` (`SortedRoomNeighbours.java:187-329`), per completed room:

| Line | Allocation / work |
|---|---|
| `:192` | `new CompleteFreeSpaceExpansionRoom(...)` |
| `:199` | `new SortedRoomNeighbours(...)` → `:54` `new TreeSet<>()`, `:55` `new LinkedList<>()` |
| `:200-201` | `new LinkedList<TreeEntry>()` + `overlappingTreeEntries` (**one 80 KB query**) |
| `:204-213` | `((LinkedList) overlappingObjects).sort(cmp)` — **`LinkedList.sort` copies to an array, sorts, then writes back through a `ListIterator`**; a comparator lambda capturing nothing but still boxing `getId()` diffs |
| `:230` | `roomShape.intersection(currentShape)` **per overlapping object** |
| `:254` | `roomShape.touchingSides(currentShape)` (1-D case) |
| `:281` | `new ExpansionDoor(completedRoom, neighbourRoom, 1)` per door |
| `:288-317` | `intersection.corner(0)`, `roomShape.equalsCorner(pt)`, `containsOnBorderLineNo(pt)` — **exact `Point` arithmetic**, see §4.8 |
| `:399-408` | `new SortedRoomNeighbour(...)` + `TreeSet.add` → `compareTo` |

`SortedRoomNeighbour.compareTo` (`:719-762`) is the sort key and it does real geometry:
`roomShape.cornerApprox(touchingSideNoOfRoom)`, then `firstCorner()` / `lastCorner()`
(`:764-805`) which call **exact** `roomShape.corner(i)` / `neighbourShape.corner(i)`. Those memoize per
`SortedRoomNeighbour` (`precalculatedFirstCorner` / `precalculatedLastCorner`, `:695-696`) but `Simplex.corner(i)`
memoizes per `Simplex` (`Simplex.java:150-164`) — and `Simplex.corner(no)` is
`lines[no].intersection(prev)`, the `BigInteger`-capable exact intersection. Tolerance constant:
`c_dist_tolerance = 1` (`:667`). Deterministic tie-break added in v2: `searchTreeObject.getId()` diff
(`:757-760`).

`calculateNewIncompleteRooms` (`:510-659`) then walks the sorted neighbours and, per gap, allocates
`Line[newEdgeLineCount]` (`:625`), `Simplex.getInstance(newEdgeLines)` (`:636`, which itself copies +
`Arrays.sort` + `removeRedundantLines`), `completedRoom.getShape().intersection(newRoomShape)` (`:639`),
an `IncompleteFreeSpaceExpansionRoom`, and an `ExpansionDoor`. In the corner-cutoff branch (`:571-582`)
it additionally allocates two `IntPoint`s, a `Line`, a half-plane `TileShape`, and **mutates the
completed room's shape** with another intersection.

`tryRemoveEdge` (`:415-507`) can restart the whole `calculate` recursively (`:112-115`) after
`removeBorderLine` + a fresh `completeShape` — an unbounded-looking recursion bounded only by the
"area must increase" check (`:483`).

`ShapeSearchTree.completeShape` (any-angle, `:580-692`) has its own two problems beyond the 80 KB stack:

1. **`Collections.sort(overlappingLeaves)` on a `LinkedList`** (`:608, :630`) — added in v2 for
   determinism ("*to ensure exact algorithmic parity with v1.9 … the non-deterministic order of tree
   traversal causes different room partitioning*", `:606-607`). v19 relied on traversal order
   (`src_v19/.../ShapeSearchTree.java:612-679`). The sort is correct; the `LinkedList` is not.
2. **O(k²) bounding-shape recomputation** (`:670-676`): inside `for (currentIncompleteRoom : result)`,
   when a restrain happens, it re-unions the **entire accumulated `newResult`**, not just the new pieces.
   The comment says "*Keep v1.9 semantics*" and v19 does the same (`src_v19/…:660-664`), so this is a
   faithful port of an upstream inefficiency, not a regression. It is safe to fix: unioning a set of
   boxes is order- and multiplicity-independent, so accumulating incrementally is bit-identical.

The 45° path (`ShapeSearchTree45Degree.completeShape`, `:96-270`) is much leaner: everything is
`IntOctagon`, `intersection` is 8 min/max (`IntOctagon.java:378-390`), the traces are behind
`debugAnchor`, and there is no `Collections.sort`. It still allocates the 80 KB stack (`:148`) and a
`LinkedList` per obstacle (`:185`). **This is the default mode** (`BoardRules.java:56`
`traceAngleRestriction = FORTYFIVE_DEGREE`; `SearchTreeManager.java:148-160` selects the tree from it).

`divideLargeRoom` (`ShapeSearchTree.java:1095-1118`) fires only when a completion produced exactly one
room covering more than half the board in both dimensions, and splits it with
`maxSectionWidth = 0.5 * max(boardHeight, boardWidth)` (`:1106`) — i.e. into ~4 pieces. Purpose is stated
at `:1091-1093`: *"Makes sure that on each layer there will be more than 1 IncompleteFreeSpaceExpansionRoom,
even if there are no objects on the layer. Otherwise, the maze search algorithm gets problems with vias."*
`TileShape.divideIntoSections` (`:900-919`) allocates the section boxes, a `LinkedList`, and does an
`intersectionWithSimplify` per box.

### 4.7 Attack surface #8 — per-expansion allocation in the maze loop

Walking `MazeSearchEngine.expandToDoorSection` (`:791-966`), the *successful* path, counting only
allocations that survive escape analysis:

| Line | Object | Bytes (approx) |
|---|---|---|
| `:853` | `FloatPoint shapeEntryMiddle` (from `middlePoint`) | 32 |
| `:857` | `FloatPoint fromMid` (bend cost, if `bendCosts[layer] > 0`) | 32 |
| `:859` | `backtrackDoor.getShape()` → whole intersection + `centreOfGravity()` `FloatPoint` | ~200–1000 |
| `:880` | `FloatPoint` (second `middlePoint` of the *same* `fromElement.shapeEntry` — recomputed, `:857` already had it) | 32 |
| `:884` | `destinationDistance.calculate(pt, layer)` → `pt.boundingBox()` = 2 × `IntPoint` + 1 × `IntBox` | 72 |
| `:889-901` | `MazeListElement` (11 fields: 2 refs, 2 ints, 2 doubles, 1 ref, 1 ref, 2 bools, 1 enum ref, 1 int) | ~80 |
| `:964` | `TreeSet.add` → `java.util.TreeMap$Entry` | 40 |
| plus | the `FloatLine shapeEntry` allocated by the caller (`:696`, `:723`, or `segmentProjection` `:197`) | 24 + 2×32 |

≈ **500 B minimum per successful expansion**, ~1.5 KB with the bend-cost `getShape()`, before the trace
strings. `DestinationDistance.calculate(FloatPoint, int)` deserves a callout:

```java
// maze/DestinationDistance.java:116-119
public double calculate(FloatPoint point, int layer) {
  return calculate(point.boundingBox(), layer);          // 3 allocations to pass 2 doubles
}
// geometry/planar/FloatPoint.java:334-338
public IntBox boundingBox() {
  IntPoint lowerLeft  = new IntPoint((int) Math.floor(this.x), (int) Math.floor(this.y));
  IntPoint upperRight = new IntPoint((int) Math.ceil (this.x), (int) Math.ceil (this.y));
  return new IntBox(lowerLeft, upperRight);
}
```

…and each `new IntPoint` runs `Math.abs(x) > Limits.CRIT_INT` twice with an `FRLogger.debug` branch
(`IntPoint.java:19-29`). The body of `calculate(IntBox, int)` (`:122-…`) only reads `box.ll.x/ll.y/ur.x/ur.y`.

`MazeListElement.compareTo` (`:79-113`) is a 4-level lexicographic comparison —
`sortingValue`, then `expansionValue`, then `door.getId()`, then `sectionNoOfDoor` — where
`door.getId()` is a virtual call that for `ExpansionDoor` is `min(id1,id2)*31 + max(id1,id2)`
(`ExpansionDoor.java:185-190`), for `ExpansionDrill` is `31*(31*location.getId() + firstLayer) + lastLayer`
(`ExpansionDrill.java:127-130`), and for `DrillPage` is `31*shape.getId() + netNumber`
(`DrillPage.java:190-193`). This runs O(log n) times per queue insert **and** per queue poll.

**Anonymous-subclass tax (v2-only).** The frontier is not a plain `TreeSet`:

```java
// MazeSearchEngine.java:84-125
mazeExpansionList = new TreeSet<>() {
  @Override public boolean add(MazeListElement element) {
    if (ctrl.isFanout && ctrl.fanoutStartPinCenter != null) {
      FloatPoint pinCenterFloat = ctrl.fanoutStartPinCenter.toFloat();          // alloc
      …
      double resolution = autorouteEngine.board.communication.getResolution(Unit.UM);   // per add()
      FloatPoint entryPoint = element.shapeEntry.a.middlePoint(element.shapeEntry.b);   // alloc
      …
    }
    return super.add(element);
  }
};
```

An override on the hot `add` that, during fanout, allocates two `FloatPoint`s and re-reads the board
resolution **on every insertion**, plus a `Math.pow`-free but still redundant unit conversion. It also
means the frontier can *silently reject* elements (returns `false` at `:106` and `:119`) — a semantic
that must be preserved: fanout escapes longer than `settings.fanout.maxEscapeLengthMm` (default 3000,
in µm × resolution) or drills closer than `minEscapeLengthMm` (default 500) are dropped at insert time.

**The `occupyNextElement` poll loop** (`:314-384`) pops from the `TreeSet` via
`Iterator it = mazeExpansionList.iterator(); listElement = it.next(); it.remove();` (`:327-329`) —
**allocating a fresh `TreeMap` iterator object on every poll**, discarding stale (already-occupied)
entries. The stale-entry drain is the standard "lazy deletion" A\* pattern and must be kept.

**Rust:** `MazeListElement` becomes a `#[derive(Clone, Copy)]` POD (`door: ExpandableId` as a packed
`u32` tag+index, `section: u16`, `backtrack: ExpandableId`, `backtrack_section: u16`,
`expansion_value: f64`, `sorting_value: f64`, `next_room: Option<RoomId>`, `shape_entry: FloatLine`
(4 × f64 inline), flags in a `u8`). That is 64–72 bytes, `Copy`, no pointers. The frontier is a
`BinaryHeap<Reverse<OrdF64Key>>` or a bucketed/radix priority queue over a `Vec`. Total heap traffic per
expansion: **zero**, amortized over the heap's `Vec` growth.

### 4.8 `geometry.planar` — allocation and `BigInteger` reachability

#### Which predicates can allocate `BigInteger`, and from where

`BigInteger` appears in exactly these files (v2 and v19 identically):
`BigIntDirection` (10 refs), `IntPoint` (14), `Line` (11), `Point` (5), `Vector` (4),
`RationalVector` (27), `RationalPoint` (43), `Limits` (2), plus `datastructures/BigIntAux` (which also
hosts the `binaryGcd` copied out of `java.math` and the 256-byte `trailingZeroTable`).

There are **two** entry points reachable from routing, and both are *conditional*:

**(a) `Line.intersection(Line other)` — `Line.java:218-304`.**
The method opens with a ladder of hand-written fast paths for orthogonal and ±45° lines
(`:225-275`), each returning a plain `new IntPoint(...)`. Only when **neither** line is axis-parallel
or diagonal does it fall through to `:277-303`:

```java
BigInteger det1 = BigInteger.valueOf(((IntPoint) a).determinant((IntPoint) b));
BigInteger det2 = BigInteger.valueOf(((IntPoint) other.a).determinant((IntPoint) other.b));
BigInteger det  = BigInteger.valueOf(delta2.determinant(delta1));
BigInteger tmp1 = det1.multiply(BigInteger.valueOf(delta2.x));
BigInteger tmp2 = det2.multiply(BigInteger.valueOf(delta1.x));
BigInteger isX  = tmp1.subtract(tmp2);
… isY …
if (signum != 0) { if (signum < 0) { negate ×3 }
  if (isX.mod(det).signum() == 0 && isY.mod(det).signum() == 0) {   // 2 mods
    isX = isX.divide(det); isY = isY.divide(det);                    // 2 divides
    if (|isX| <= Limits.CRIT_INT && |isY| <= Limits.CRIT_INT) return new IntPoint(...);
    det = BigInteger.ONE; } }
return new RationalPoint(isX, isY, det);
```

That is **~15 `BigInteger` allocations plus 2 `mod` and 2 `divide`** — and, when the result is not
integral, a `RationalPoint` whose every subsequent `equals`, `compareX`, `compareY`, `sideOf`, `translateBy`
is `BigInteger` arithmetic (`RationalPoint.java`, 43 `BigInteger` references) and whose `addTo`/`add` go
through `BigIntAux.addRationalCoordinates` (`BigIntAux.java:42-62`, allocating a `BigInteger[3]` plus up
to 5 products/sums).

**Reachability from the maze loop.** `Simplex.corner(int)` (`Simplex.java:139-164`) is
`precalculatedCorners[no] = lines[no].intersection(prev)`. `corner()` is called from:
`SortedRoomNeighbours.compareTo → firstCorner()/lastCorner()` (`:768-800`) — i.e. **inside the
`TreeSet` sort during every room completion**; `SortedRoomNeighbours.insertDoorOk` (`:349, 352`);
`SortedRoomNeighbours.calculateNeighbours:288` (`intersection.corner(0)`);
`ExpansionDoor.calcDoorLineSegment:156`; `MazeRipupResolver.enterThroughSmallDoor` (via
`doorShape.corner_approx` — that one is the *approximate* variant, safe). Therefore:

> **In any-angle (`AngleRestriction.NONE`) mode, `BigInteger` arithmetic is on the room-completion hot
> path, gated only by whether the room's border lines happen to be axis-parallel or diagonal.** Since
> room borders derive from trace offset shapes, arbitrary-direction lines are the norm there.
> In the default 45° mode the fast paths in `Line.intersection` catch essentially everything, and
> `IntOctagon`-based shapes never construct `Simplex` corners at all — so 45° routing is effectively
> `BigInteger`-free.

**(b) `IntPoint.perpendicularProjection(Line)` — `IntPoint.java:135-176`.**
Unconditionally `BigInteger`: 7 `valueOf`s, 4 `multiply`s, 4 `add`/`subtract`s, then `2 × mod` and
`2 × divide`, returning either an `IntPoint` or a `RationalPoint`. It has **no** fast path. Reached via
`Line.perpendicularProjection(Point)` (`Line.java:339-341`) and `Point.perpendicularProjection`, which
**[PULL]** uses in the tightening rules. Note the deliberately non-allocating twin
`FloatPoint.projectionApprox(Line)` (`FloatPoint.java:192-196`) exists for the hot cases.

`Line.sideOfIntersection(p1, p2)` (`Line.java:152-164`) is the two-tier design mentioned in §1.4: it
first tries `p1.intersectionApprox(p2)` (pure `double`, one `FloatPoint`) with tolerance **1.0**, and
falls back to the exact `p1.intersection(p2)` only on `COLLINEAR`. It is called from
`Simplex.removeRedundantLines` (`:927`), so every degenerate/near-degenerate triple during shape
simplification can trip the `BigInteger` path.

#### `Simplex` intersection cost, in full

```java
// Simplex.java:619-630
public Simplex intersection(Simplex other) {
  if (this.isEmpty() || other.isEmpty()) return EMPTY;
  Line[] newArray = new Line[lines.length + other.lines.length];     // alloc 1
  System.arraycopy(this.lines,  0, newArray, 0,            lines.length);
  System.arraycopy(other.lines, 0, newArray, lines.length, other.lines.length);
  Arrays.sort(newArray);                                             // O(n log n) Line.compareTo
  Simplex result = new Simplex(newArray);                            // alloc 2
  return result.removeRedundantLines();                              // alloc 3 (Line[]) + 4 (Side[]) + 5 (Simplex)
}
```

`Arrays.sort(Line[])` is a **TimSort on objects**, which allocates a merge run array for n ≥ 32 and
invokes `Line.compareTo` (`Line.java:425-471`: 4 casts, 4 int subtractions, a branch ladder, then a
`double` determinant). Both inputs are *already sorted* — a merge would be O(n) and allocation-free.

`removeRedundantLines` (`:884-1010`) then: copies into a fresh `Line[]` skipping `fastEquals`
duplicates; allocates `Side[newLength]`; runs a `while (tryAgain)` loop that repeatedly scans the ring,
calling `prevLine.direction()` / `nextLine.direction()` (each a lazily-cached `Direction` on the `Line`,
but `IntDirection.getVector()` allocates a **fresh `IntVector` every call**, `IntDirection.java:36-38`),
`determinant`, and `currentLine.sideOfIntersection(prevLine, nextLine)` when the memo slot is null.
Line removal does an `System.arraycopy`-free manual shift of both arrays. Worst case is O(k²)
`sideOfIntersection` calls.

#### Other per-call object churn in `geometry.planar`

- `IntDirection.getVector()` → `new IntVector(x, y)` every call (`IntDirection.java:36-38`); called from
  `Direction.sideOf` (**2 allocations**, `Direction.java:113-115`), `Direction.projection` (**2**,
  `:122-124`), `Direction.equals` (**2**, `:102-104`), `Line.isPerpendicular` (**2**, `Line.java:401-403`),
  `Line.translate` (`:352`), `Simplex.cornerIsBounded` (**2**, `Simplex.java:103-105`),
  `FloatPoint.roundToTheRight/Left` (`FloatPoint.java:111, 159` — plus a `toFloat()` `FloatPoint`).
- `IntDirection.turn45Degree(factor)` allocates a new `IntDirection` for **all 8 cases including
  `factor % 8 == 0`** (`IntDirection.java:101-114`); `IntVector.turn90Degree` likewise (`:69-84`).
- `Direction.middleApprox` allocates 2 `FloatPoint`s + 1 `IntVector` + runs `binaryGcd`
  (`Direction.java:127-137` → `IntVector.toNormalizedDirection` `:194-205`).
- `Line.opposite()`, `translateBy`, `turn90Degree`, `mirrorVertical/Horizontal` each allocate a new
  `Line` + up to 2 `Point`s (`Line.java:210-212, 369-377, 539-558`).
- `TileShape.nearestBorderPointsApprox(from, count)` allocates `FloatPoint[count]` + `double[count]`
  and materializes `cornerApprox(i)` for every border line (`TileShape.java:378-…`); called from
  `MazeSearchEngine.java:467` and `:735` in the thickness tests, per expansion.
- `Simplex`'s memo fields are `transient` (`:22-26`) — so a `deepCopy`'d board loses all of them.

**Rust:** `IntPoint`/`IntVector`/`IntDirection`/`FloatPoint`/`FloatLine`/`IntBox`/`IntOctagon` are all
`#[derive(Clone, Copy)]` value types — zero allocation, and `Direction::vector()` returns by value.
`Simplex` becomes `{ lines: ArrayVec<Line, 16>, cache: Cell<ShapeCache> }` or an arena-allocated
`&[Line]`. `intersection` merges two sorted slices into a stack buffer. For exact arithmetic, replace
`BigInteger` with `i128` (`determinant` of two `i32`-bounded coordinates fits in `i64`; the products in
`Line::intersection` fit comfortably in `i128` given `Limits::CRIT_INT`) and represent the rational
fallback as `struct RationalPoint { x: i128, y: i128, den: i128 }` — `Copy`, no allocation, and the
`mod`/`divide` become native `i128` ops. **Verify the bound**: `Limits.CRIT_INT` caps coordinates
(`IntPoint.java:20, 23`), so `det1 * delta2.x` is bounded by `2·CRIT_INT² · CRIT_INT` — check this
against `Limits.java` before committing to `i128`, and keep an `i256`/`num-bigint` escape hatch behind
a `debug_assert`.

### 4.9 Attack surface #9 — collection churn

Per **expansion** (`MazeSearchEngine.expandToRoomDoors`):
- `:559` `new LinkedList<>(listElement.nextRoom.getDoors())` — a full copy of the door list, as a
  linked list, to guard against concurrent modification during iteration. Then `:571` iterates it by
  **index** (`roomDoorsSnapshot.get(doorIndex)`) — **O(D²) pointer chasing on a `LinkedList`**.
- `:481-483` `new ItemSelectionFilter(CONDUCTION)` + `board.pickItems(...)` when the layer is inactive.
- `:672, :675` `new int[1]` + `new IntPoint[2]` + `new Polyline(checkPoints)` per target door when the
  room is thin.
- `:1140, :1172` `new LinkedList<MazeTraceShover.DoorSection>()` per shove attempt (both sides).

Per **room completion**: `new TreeSet<SortedRoomNeighbour>` + `new LinkedList<TreeEntry>` (×2, one for
`ownNetObjects`) + one `LinkedList` per obstacle inside `completeShape`.

Per **spatial query**: see §4.1.

Per **connectivity walk**: `new TreeSet<>()` ×3 per `getNormalContacts` call
(`Trace.java:156, 174, 178` — and `:174` returns an empty `TreeSet` just to signal "not an endpoint").

Per **ripup check** (v19, `MazeSearchAlgo.java:1462-1503`): a `StringBuilder` accumulating every
connection item id, plus `new int[netCount]` and `Arrays.toString(nets)` — **all purely to build a
`FRLogger.trace` argument** (`:1504-1514`). v2's `MazeRipupResolver` dropped that particular
StringBuilder, which is one of the few v19→v2 *improvements*.

`ItemSearchTreesInfo` (`ItemSearchTreesInfo.java:12-92`) stores per-item, per-tree caches in a
`LinkedList<SearchTreeInfo>` and looks them up by **linear scan with `==` on the tree object**
(`:25-32, :51-58`). Every `Item.getTreeShape(tree, i)` (`Item.java:213-226`) goes through
`getPrecalculatedTreeShapes` (`:228-238`) → that scan, plus an implicit iterator allocation. With 2–3
trees the scan is short, but `getTreeShape` is called once per tree entry in every spatial-query result
loop, i.e. tens of millions of times.

**Rust:** `SmallVec<[DoorId; 8]>` for door lists (rooms rarely have many doors); index-based iteration
over a slice; per-item tree caches as a fixed `[Option<Box<[TileShape]>>; MAX_TREES]` array indexed by
a `TreeId: u8`; the connectivity sets as `SmallVec<[ItemId; 8]>` + sort/dedup instead of `BTreeSet`.

### 4.10 Attack surface #4 — `deepCopy` and the MT optimizer

```java
// board/facade/RoutingBoardUndoFacade.java:41-72
synchronized RoutingBoard deepCopy() {
  ByteArrayOutputStream byteArrayOutputStream = new ByteArrayOutputStream();
  ObjectOutputStream outputStream = new ObjectOutputStream(byteArrayOutputStream);
  outputStream.writeObject(board);  outputStream.flush();
  ByteArrayInputStream in = new ByteArrayInputStream(byteArrayOutputStream.toByteArray());
  ObjectInputStream inputStream = new SafeObjectInputStream(in);
  RoutingBoard boardCopy = (RoutingBoard) inputStream.readObject();
  boardCopy.clearAllItemTemporaryAutorouteData();
  boardCopy.finishAutoroute();
  return boardCopy;
}
```

Cost, per call:
1. **Reflective serialization** of the entire object graph (items, polylines, rules, components,
   netlists). Java serialization is ~10–50× slower than a memcpy and allocates per object.
2. `ByteArrayOutputStream` grows by doubling → the copy transiently holds ~2× the serialized size, and
   `.toByteArray()` (`:51`) makes **another full copy**. So peak = 3× serialized size, in byte arrays.
3. **Reflective deserialization** — allocates every object again.
4. `BasicBoard.readObject` (`:1388-1400`) then does:
   ```java
   searchTreeManager = new SearchTreeManager(this);          // fresh default tree
   for (Item currentItem : this.getItems()) { currentItem.board = this; searchTreeManager.insert(currentItem); }
   ```
   — a **full R-tree rebuild**, and each `insert` triggers `calculateTreeShapes` for the item (offset
   shapes per polyline segment, **[TREE]** §3), because `searchTreesInfo` was reset.

`searchTreeManager` and the other derived caches are `transient` (`BasicBoard.java:94-103`), which is
correct for wire size but means every copy pays the full index rebuild.

**Where it is called:**

| Site | Frequency |
|---|---|
| `OptimizeRouteTask.java:30` (in the **constructor**) | **once per item to optimize** |
| `AutoroutePassRunner.java:55` | once per router thread per MT autoroute pass |
| `AutorouteConnectionRouter.java:84-85` `router.board.serialize(false)` | once per connection **if `isStrictDrc()`** |

The `OptimizeRouteTask` case is the killer, because of the submission loop:

```java
// BatchOptimizerMultiThreaded.java:249-260
// We schedule just enough tasks to keep workers busy in order not to exhaust JVM memory so
// that it can run on systems without huge amount of RAM using the pool
for (int t = 0; t < itemIds.size(); t++) {
  int itemId = itemIds.get(t);
  job.logDebug("Scheduling task #" + (t + 1) + " of " + itemIds.size() + " for item #" + itemId + ".");
  OptimizeRouteTask newTask = new OptimizeRouteTask(this, this.job, itemId, passNo, withPreferredDirections);
  pool.execute(newTask);
}
```

Every task is constructed — and therefore **deep-copies the board** — on the *submitting* thread,
before being queued. `itemIds.size()` is the number of routed items on the board (thousands). All those
board copies are strongly reachable from the `ThreadPoolExecutor`'s unbounded `LinkedBlockingQueue`
until their task runs. **That is the ~1 GB peak heap**, and it directly contradicts the comment above it.
`OptimizeRouteTask.clean()` (`:97-100`) nulls `itemToOptimize.board` and `itemToOptimize` to "speed up
memory release" — a band-aid that does not release `this.board`.

`replaceMasterRoutingBoardWithTheWinningCandidate()` (`:147-156`) then *adopts the winner's cloned
board* as the master (`this.board = winningCandidate.board;`) — so under `GREEDY` the copies are not
merely scratch, they are the new state. That semantics must be preserved (**[BATCH]** §8.2).

**Rust:** the optimizer's "try this alternative and keep it if better" is a *speculative branch on a
persistent structure*, not a copy. Options, in order of preference:
1. **Undo log.** Record item insert/remove/replace operations; roll back on reject. The board already
   has `UndoableObjects` (`board.itemList.generateSnapshot()/popSnapshot()`,
   `BoardSnapshotManager.java:75-83`) — upstream *has* the machinery and does not use it here.
2. **Copy-on-write items.** `Arc<Item>` in a `Vec<Arc<Item>>`; a clone copies `Vec` of pointers plus
   the R-tree structure (which is itself an arena of `u32`s — a `Vec::clone`, i.e. one `memcpy`).
3. If a true snapshot is needed: arena `memcpy` (`Vec<Item>` of POD-ish items + `Vec<Node>` for the
   tree) — nanoseconds per MB, no reflection, no index rebuild.
Also: **bound the queue.** Submit at most `2 × threadPoolSize` tasks at a time and construct the
snapshot *inside* `run()`, not in the constructor.

### 4.11 Attack surface #6 — connectivity is recomputed geometrically

There is **no adjacency cache**. Contacts are discovered by point-location queries every time.

```java
// board/model/items/Trace.java:173-201
public Set<Item> getNormalContacts(Point point, boolean ignoreNet) {
  if (point == null || !(point.equals(this.firstCorner()) || point.equals(this.lastCorner())))
    return new TreeSet<>();
  TileShape searchShape = TileShape.getInstance(point);                   // alloc
  Set<SearchTreeObject> overlaps = board.overlappingObjects(searchShape, this.layer);   // 80 KB QUERY
  Set<Item> result = new TreeSet<>();                                     // alloc
  for (SearchTreeObject o : overlaps) { … }
  return result;
}
// :155-166
public Set<Item> getNormalContacts() {          // one query per endpoint
  Set<Item> result = new TreeSet<>();
  result.addAll(getNormalContacts(firstCorner(), false));
  result.addAll(getNormalContacts(lastCorner(),  false));
  return result;
}
```

**One `getNormalContacts()` = 2 spatial queries = ~160 KB of garbage.**

It is the primitive for:

| Consumer | Site | Amplification |
|---|---|---|
| `Item.getConnectedSet(net)` | `Item.java:606-635` | one call **per item in the connected component** (DFS) |
| `Item.getUnconnectedSet(net)` | `:676-690` | `board.getConnectableItems(net)` (**full O(N) board scan**, `BoardConnectivityQueries.java:23-35`) **plus** a `getConnectedSet` |
| `Connection.get(item)` | `path/Connection.java:38-…` | walks the connection with a `getNormalContacts` per hop, plus an extra `getNormalContacts(point,false)` fork test per contact (`:66`) |
| `MazeRipupResolver.checkRipup` | via `Connection.get` and `calcFanoutViaRipupCostFactor` (`:38-40` `getStartContacts`/`getEndContacts`) | **per obstacle room entered during maze search** |
| `Item.getConnectionItems(stopOption)` | `Item.java:702-…` | used when deleting ripped connections |
| `BatchAutorouter.getAutorouteItems(board)` | `:345-380` | see below |

`Connection.get` is memoized in `ItemAutorouteInfo.precalculatedConnection` (`:17, 44-51`) — but that
memo is cleared per connection by `clearAllItemTemporaryAutorouteData()` / `resetAllDoors()` (§4.4).

**`getAutorouteItems` is the quadratic pass-startup.** Once per pass (`AutoroutePassRunner.java:157`,
`:15`):

```java
for each object in board.itemList:                                    // full scan
  if (object instanceof Connectable && object instanceof Item item && !item.isRoutable())
    if (!handledItems.contains(item))
      for i in 0..item.netCount():
        net = item.getNetNumber(i)
        connectedSet = item.getConnectedSet(net)                      // DFS with 2 queries per item
        for c in connectedSet: if c.netCount() <= 1: handledItems.add(c)
        netItemCount = board.connectableItemCount(net)                // ANOTHER FULL O(N) BOARD SCAN
        if (connectedSet.size() < netItemCount && !item.hasIgnoredNets()) …
```

For P pins and N items this is O(P·N) list traversal plus O(Σ|connectedSet|) spatial queries — per pass.
On a 418-net board with ~50 passes this is a first-order cost, and the `reusableAutorouteItemList` /
`reusableHandledItems` optimization (`BatchAutorouter.java:70-73, 347-348`) only saves the two output
containers.

**Rust:** maintain the connectivity graph *incrementally*.
- A `HashMap<(IVec2, LayerId), SmallVec<[ItemId; 4]>>` endpoint index, updated on item insert/remove —
  contacts become a hash lookup, not a tree query.
- A per-net union-find over item ids, updated on insert/remove, giving `connected_set_size(net)` and
  "is this net complete?" in O(α). That kills `getUnconnectedSet`, `connectableItemCount`,
  `calculateIncompleteCount`, and the `getAutorouteItems` scan in one stroke.
- A per-net `Vec<ItemId>` maintained on insert/remove, replacing `getConnectableItems`'s full scan.
Preserve the *semantics* of "normal contact" exactly (`Trace.normalContactPoint`, `Trace.java:130-153`:
same layer, endpoint coincidence, and the "not both corners" degenerate rejection).

### 4.12 Attack surface #10 — statistics and ratsnest inside the pass loop

`BatchAutorouter.calculateIncompleteCount(board)` (`:556-564`) constructs a `DesignRulesChecker` and
calls `calculateAllIncompletes()` (`drc/DesignRulesChecker.java:542-…`), which:
- allocates a `Vector<Collection<Item>>` of size `maxNetNumber` with a `LinkedList` per net (`:545-548`);
- does a **full scan of `board.itemList`** to bucket items by net (`:549-…`);
- computes the per-net ratsnest (minimum spanning structure) for every net.

Call sites per pass (`AutoroutePassRunner.runSingleThread`):
`:180-183` (start of pass), `:299` and `:303` (before/after `removeTails`, inside `logTailRemoval`
whose arguments are eagerly evaluated), `:314` (end of pass), and `:511` inside `updateProgress`
whenever `router.shouldFireBoardUpdate()` is true.

`new BoardStatistics(router.board, null, false)` — another full board traversal — runs at `:174`
(start of pass), `:305` (end), and every `PROGRESS_STATISTICS_ITEM_INTERVAL = 10` routed items
(`:497-505`, guarded by the constant declared at `BatchAutorouter.java:57` with the "expensive
snapshot" comment).

**Rust:** all of these become O(1) reads off the incremental union-find + running counters from §4.11.
Compute a full `BoardStatistics` only when the scoring function actually needs it (**[BATCH]** §6).

---

## 5. Constants, thresholds, and limits relevant to cost

| Constant | Value | Site | Why it matters for cost |
|---|---|---|---|
| `ArrayStack` initial capacity | **10 000** | `MinAreaTree.java:30`; `ShapeSearchTree.java:609`; `…45Degree.java:148`; `…90Degree.java:69` | 40–80 KB zeroed per spatial query |
| `ArrayStack` growth | ×4 | `ArrayStack.java:46` | never hit in practice |
| `AutorouteEngine.TRACE_WIDTH_TOLERANCE` | **2** | `AutorouteEngine.java:41` | added to half-width in door sizing |
| `maxDrillPageWidth` | `max(5 × defaultViaDiameter, 10000)` | `AutorouteEngine.java:89-90` | sets drill-page grid density |
| `maxDoorSectionWidth` | `10 × offset` | `ExpansionDoor.java:138` | sections per door → queue entries per door |
| 2-D door minimum size | `distanceSquare < 4·offset²` ⇒ 0 sections | `ExpansionDoor.java:128-131` | |
| `divideLargeRoom` threshold | room > ½ board in **both** dims | `ShapeSearchTree.java:1102-1105` | |
| `divideLargeRoom` section width | `0.5 × max(boardH, boardW)` | `:1106` | |
| `maxTreeShapeWidth` | **50 000**, or `min(500 × resolution(MIL), 50000)` with a host CAD | `ShapeSearchTree.java:916-920` | splits big obstacle areas into tree shapes |
| per-connection `TimeLimit` | `min(100000 × 2^(pass−1), Integer.MAX_VALUE)` ms | `AutorouteConnectionRouter.java:73-75`; v19 `BatchAutorouter.java:654-656` | **100 s for a single connection on pass 1** — this is why 300 s suite timeouts happen |
| `PROGRESS_STATISTICS_ITEM_INTERVAL` | **10** | `BatchAutorouter.java:57` | full board stats every 10 items |
| `STAGNATION_SCORE_THRESHOLD` | **0.5** (on a 0–1000 scale) | `BatchAutorouter.java:60` | see **[BATCH]** §5 |
| `MAX_RIPUP_COSTS` | `Integer.MAX_VALUE / 100` | v19 `MazeSearchAlgo.java:1493` | |
| `ALREADY_RIPPED_COSTS` | **1** | `MazeSearchEngine.java:44` | |
| `FANOUT_COST_CONSTANT` | **20 000** | `MazeRipupResolver.java:27` | |
| `SortedRoomNeighbour.c_dist_tolerance` | **1** | `SortedRoomNeighbours.java:667` | |
| `sideOfIntersection` tolerance | **1.0** | `Line.java:155` | wider ⇒ fewer exact (BigInteger) fallbacks |
| default `AngleRestriction` | **FORTYFIVE_DEGREE** | `BoardRules.java:56` | selects the cheap `IntOctagon` code paths |
| default `retainAutorouteDatabase` | **false** | `BatchAutorouter.java:154`; v19 `:102` | §4.4 |
| default `strictDrc` | **false** (`null`) | `RouterSettings.java:52, 533` | when true: full board `serialize()` per connection |
| fanout escape gate | `maxEscapeLengthMm` default **4.5** mm, `minEscapeLengthMm` default **2.5** mm (release `DefaultSettings.java:117-118`; the 3000/500 µm figures previously quoted here came from the git-master source tree and survive in the release only as hardcoded fallbacks when `settings.fanout == null`) | release `MazeSearchAlgo.java:80-108` (jar class `MazeSearchAlgo$1`); git-master `MazeSearchEngine.java:96-120` | frontier `add()` can silently reject |
| `Simplex` memo fields | `transient` | `Simplex.java:22-26` | lost across `deepCopy` |

---

## 6. v1.9 → v2 cost deltas (2.3.0 is the v2 column)

| Area | v1.9 | v2 (2.3.0) | Verdict |
|---|---|---|---|
| R-tree traversal stack | reusable field, `reset()` per query (`MinAreaTree.java:19,32`; `ShapeSearchTree.java:607`; `…45Degree.java:125`; `…90Degree.java:67`) | `new ArrayStack<>(10000)` per query at 4 sites | **regression** — the single largest one |
| `completeShape` obstacle order | tree traversal order | `Collections.sort(new LinkedList<Leaf>)` for determinism (`ShapeSearchTree.java:604-630`) | **necessary but poorly implemented** (sort a `Vec`, not a list) |
| `completeShape` O(k²) bbox union | present (`src_v19/…:660-664`) | present (`:670-676`) | **inherited**, safe to fix |
| `checkRipup` diagnostics | `StringBuilder` of connection item ids + `Arrays.toString(nets)` per call (`MazeSearchAlgo.java:1462-1503`) | removed | **improvement** |
| Maze frontier | plain `TreeSet<MazeListElement>` (`MazeSearchAlgo.java:66`) | anonymous `TreeSet` subclass overriding `add()` with fanout length gates (`MazeSearchEngine.java:84-125`) | **regression** (per-`add` allocations + a board-settings read) |
| Bend cost | absent | `expandToDoorSection:855-873` — `backtrackDoor.getShape().centreOfGravity()` per expansion when `bendCosts[layer] > 0` | **new feature with a new cost**; make the door shape cached (§4.3) and it is free |
| `AutorouteControl` net init | no `isPureSmdNet` | `isPureSmdNet` → `board.getConnectableItems(net)`, a full board scan, per connection (`AutorouteControl.java:189-190, 263`) | **regression** |
| `expandToTargetDoors` | direct `getTraceConnectionShape` | adds a `treeShapeCount` bounds check + null check per target door (`MazeSearchEngine.java:653-668`) | **robustness fix**, negligible cost |
| `ItemAutorouteInfo.getExpansionRoom` | fixed array | resizes + `System.arraycopy` when the item's tree-shape count changed (`ItemAutorouteInfo.java:57-66`) | **robustness fix** |
| Structure | one 1729-line `MazeSearchAlgo` | split into `MazeSearchEngine` / `MazeExpansionEngine` / `MazeRipupResolver` / `MazeTraceShover` / `MazeFanoutDiagnostics` | neutral for cost; better for a port |
| Board deep copy | Java serialization | Java serialization (unchanged) | inherited |
| Debug hardcodes | `"U27-"` pin prefix in `MazeSearchAlgo.java:1042-1043` | `MazeFanoutDiagnostics.java:17`; net `33/66/67` at `AutorouteEngine.java:164`; net 77 + literal coords at `ShapeSearchTree45Degree.java:549-554` | both trees; strip in Rust |

---

## 7. Call graph (cost view)

Inbound to the area this document audits:

```
RoutingPipeline                                                        [BATCH §1.1]
  └─ AutorouteBatchLoop.run()                                          [BATCH §5.2]
       ├─ BatchFanout (fanout stage; the ONLY stage with allocation telemetry, §1.2)
       └─ per pass: AutoroutePassRunner.runSingleThread(passNo)
            ├─ BatchAutorouter.getAutorouteItems(board)          ── §4.11 (O(P·N) + queries)
            ├─ new BoardStatistics(board, …)                     ── §4.12 (full scan)
            ├─ DesignRulesChecker.calculateAllIncompletes()      ── §4.12 (full scan + ratsnest)
            └─ per item: AutorouteConnectionRouter.route(...)
                 ├─ new AutorouteControl(board, net, …)          ── §4.9 (7 arrays + O(L²)) + isPureSmdNet full scan
                 ├─ item.getUnconnectedSet(net) / getConnectedSet ── §4.11 (2 queries per item)
                 ├─ [if strictDrc] board.serialize(false)        ── §4.10 (full serialization)
                 ├─ board.initAutoroute(..., retain=false)       ── §4.4 (NEW AutorouteEngine + DrillPageArray)
                 ├─ AutorouteEngine.autorouteConnection(...)
                 │    ├─ MazeSearchEngine.getInstance → init()   ── start rooms, completeExpansionRoom
                 │    ├─ findConnection() → occupyNextElement()* ── §4.7 THE LOOP
                 │    │     ├─ expandToRoomDoors                 ── §4.2 traces, §4.3 getShape, §4.6 completion
                 │    │     │    ├─ AutorouteEngine.completeNeighbourRooms
                 │    │     │    │     └─ completeExpansionRoom → ShapeSearchTree.completeShape  ── §4.1, §4.6
                 │    │     │    │           └─ SortedRoomNeighbours.complete → 45°/90°/any-angle ── §4.6, §4.8
                 │    │     │    ├─ expandToTargetDoors → board.checkForcedTracePolyline          ── [SHOVE]
                 │    │     │    ├─ MazeRipupResolver.checkRipup → Connection.get                  ── §4.11
                 │    │     │    ├─ shoveTraceRoom → MazeTraceShover.checkShoveTraceLine           ── [SHOVE §10]
                 │    │     │    ├─ expandToDoor → ExpansionDoor.getSectionSegments                ── §4.3
                 │    │     │    │     └─ expandToDoorSection → DestinationDistance.calculate      ── §4.7
                 │    │     │    └─ drillPageArray.overlappingPages → expandToDrillPage            ── §4.5
                 │    │     ├─ expandToDrillsOfPage → DrillPage.getDrills                          ── §4.5 (splitToConvex!)
                 │    │     └─ expandToOtherLayers → ForcedViaInserter.checkLayer                   ── [SHOVE §8]
                 │    ├─ FoundConnectionLocator.getInstance                                        ── [PULL §2-3]
                 │    ├─ AutorouteEngine.clear() | resetAllDoors()                                  ── §4.4
                 │    └─ FoundConnectionInserter.getInstance                                        ── [PULL §4]
                 └─ board.optChangedArea(...)                                                       ── [PULL §5]
       └─ BatchOptimizer / BatchOptimizerMultiThreaded                                              ── §4.10, [BATCH §7-8]
            └─ per item: new OptimizeRouteTask(...) → job.board.deepCopy()                          ── §4.10
```

Outbound (everything the audited code depends on):
`geometry.planar` (§4.8), `datastructures.{MinAreaTree, ShapeTree, ArrayStack, UndoableObjects}` (§4.1, §4.9),
`board.searchtree.*` (§4.1, §4.6), `board.model.items.*` (§4.11), `logger.FRLogger` + `debug.DebugControl` (§4.2),
`drc.DesignRulesChecker` (§4.12), `core.scoring.BoardStatistics` (§4.12).

---

## 8. A measurement plan for the Rust port

The Java profiler (§1) is not a model to copy. Instrument the Rust port as follows so that the
comparison against the Java baseline is meaningful:

1. **Allocation counting first.** Wrap the global allocator in a counting shim behind a `--features
   alloc-stats` build. Target: **zero heap allocations per maze expansion** and **zero per spatial
   query** in steady state. If a benchmark shows non-zero, that is a bug, not a tuning opportunity.
2. **Counters, not timers, in the inner loop.** `expansions`, `queue_pushes`, `queue_pops`,
   `stale_pops`, `tree_queries`, `tree_nodes_visited`, `room_completions`, `door_shape_computations`,
   `exact_intersections` (the `i128` path), `shove_checks`, `ripup_checks`. Plain `u64` fields on a
   per-thread struct; `#[cold]` reporting. These reproduce what `PerformanceProfiler` *should* have
   measured.
3. **Timers only at stage boundaries** — mirror the eight `profile*Nanos` counters of §1.3 so the
   stage-level split is directly comparable to `-Dfreerouting.benchmark.profile=true` runs.
4. **Keep upstream's pass history** (`passNo`, `unroutedItems`, `durationMs`, `ripupCost`) — it is the
   cheapest signal for "is my search behaving like the reference?" (**[BATCH]** §4.2).
5. **Differential harness.** Run the Java with `-Dfreerouting.benchmark.profile=true
   -Dfreerouting.benchmark.retain_autoroute_database=false` and compare *route-for-route*, not just
   scores. Retention (§4.4) is the first thing that will diverge; keep a `fresh_database` mode to
   isolate it.
6. **Watch the tail, not the mean.** 14/46 fixtures time out at 300 s, and a *single connection* is
   allowed 100 s on pass 1 (`AutorouteConnectionRouter.java:73-75`). Report p99 per-connection time and
   per-connection expansion counts; a pathological connection is what kills the suite, not the average.

---

## 9. Rust design implications

### 9.1 Keep semantically (do not "optimize" these away)

- **Lazy deletion in the frontier.** `occupyNextElement` pops until it finds a non-occupied door
  section (`MazeSearchEngine.java:322-338`). A `BinaryHeap` with a stale-entry check reproduces this
  exactly; a decrease-key structure does **not** (it changes which duplicate wins).
- **The full 4-level `MazeListElement` ordering**: `sortingValue`, `expansionValue`, `door.getId()`,
  `sectionNoOfDoor` (`MazeListElement.java:79-113`). The id tie-breaks are what make v2 deterministic;
  drop them and routes drift. Reproduce each `getId()` formula bit-exactly
  (`ExpansionDoor.java:185-190`, `ExpansionDrill.java:127-130`, `DrillPage.java:190-193`,
  `Simplex.java:70-77`, `IntPoint.java:126-128`, `Line.java:53-55`).
- **`Leaf` ordering for tree query results** (`ShapeTree.java:217-224`) and the
  `Collections.sort(overlappingLeaves)` in `completeShape` (`ShapeSearchTree.java:630`) — obstacle
  visit order determines room partitioning.
- **`SortedRoomNeighbour.compareTo`** in full, including the `c_dist_tolerance = 1` band, the
  `neighbourRoomTouchIsCorner` direction comparison, and the `searchTreeObject.getId()` tie-break
  (`SortedRoomNeighbours.java:719-762`).
- **The fanout frontier gates** (`MazeSearchEngine.java:88-122`) — rejecting an element at insert time
  is load-bearing behavior, not a diagnostic.
- **`sideOfIntersection`'s two-tier float-then-exact structure** with tolerance `1.0`
  (`Line.java:152-164`). Change the tolerance and shapes change.
- **`insertDoorOk`'s "same net only" 2-D overlap-door rule** (`SortedRoomNeighbours.java:336-345`) and
  the "trace door must be parallel to the trace line at the first/last segment" rule (`:375-389`) —
  both are cost-motivated prunings that affect results.
- **Time limits** (`100000 × 2^(pass−1)` ms) and **`TRACE_WIDTH_TOLERANCE = 2`**, and every constant in §5.

### 9.2 Restructure

**Arena everything; index with integers.**
`Room`, `Door`, `Drill`, `DrillPage`, `Item`, and R-tree `Node` all live in `Vec`s; references are
`u32` newtypes. This kills the pointer chasing that dominates `LinkedList`/`TreeSet` traversal, makes
snapshotting a `memcpy`, and makes "clear the autoroute database" a `Vec::clear()` rather than N R-tree
deletions (`AutorouteEngine.java:308-312`).

**Value-type geometry.** `IntPoint`, `IntVector`, `IntDirection`, `FloatPoint`, `FloatLine`, `IntBox`,
`IntOctagon`, `Line`, `Side`, `Signum`: all `Copy`, all passed by value, none allocating. `Simplex`
becomes `ArrayVec<Line, N>` (N=16 covers essentially every real shape; spill to an arena slice beyond).
Replace the `Point`/`Vector` abstract-class hierarchy and its double-dispatch
(`Point.differenceBy(Point)` → `other.differenceBy(this)` → `.negate()`, `IntPoint.java:96-110`) with a
single `enum Point { Int(IntPoint), Rational(RationalPoint) }` and direct matches — the Java's
visitor-style dispatch costs two virtual calls and an extra allocation per subtraction.

**`i128` instead of `BigInteger`.** §4.8. Bound-check against `Limits::CRIT_INT` in `debug_assert!`.
Make `RationalPoint` a `Copy` struct.

**Cache the derived, invalidate by dirty rect.**
- Door shape and section segments: computed once at door creation (§4.3).
- Drill page decomposition: net-independent part cached for the session (§4.5).
- Expansion rooms: persist across connections; repair incrementally with
  `removeCompleteExpansionRoom`'s algorithm (`AutorouteEngine.java:377-412`) (§4.4).
- Item tree shapes: `[Option<Box<[TileShape]>>; MAX_TREES]` indexed by `TreeId`, not a `LinkedList`
  linear scan (§4.9).
- Connectivity: endpoint hash index + per-net union-find, maintained on mutation (§4.11).

**Scratch buffers, not allocations.** A per-thread `Scratch { node_stack: Vec<NodeIdx>, leaves:
Vec<LeafId>, entries: Vec<TreeEntry>, lines: Vec<Line>, doors: Vec<DoorId> }` passed by `&mut` into every
query. `clear()` at entry, never shrink. This is the direct fix for #1, #7, #9 simultaneously.

**Diagnostics that cost nothing when off.** `tracing` macros or `#[cfg(feature)]`. Enforce with a test
that the release binary contains no `format!` in the maze module.

**Parallelism.** The route stage is strictly sequential in both v19 and v2 (**[BATCH]** §9) — every
expansion mutates the shared board and search tree. Do **not** try to parallelize inside a connection.
The tractable parallelism is:
1. **The optimizer** (§4.10), where speculative alternatives are independent — but with structural
   sharing instead of deep copies, and a bounded task queue.
2. **The MT autoroute pass** (`AutoroutePassRunner.runMultiThread`, `:11-…`), which is
   "N independently shuffled attempts, keep the best board" — embarrassingly parallel *if* board
   snapshots are cheap. With `Arc`-based COW this becomes viable at a much larger N.
3. **Read-only spatial queries** during a pass, if the router is restructured to batch them (e.g.
   completing all neighbour rooms of a frontier region at once). This is a redesign, not a port.

**Do not port**: the `PerformanceProfiler` nesting-stack design (§1.1), the anonymous `TreeSet`
subclass (§4.7), the `try/catch(RuntimeException)` in `safeMazeSectionCount`
(`MazeSearchEngine.java:252-258`), the hardcoded net/pin debug anchors (§0.2), and Java serialization
as a copy mechanism (§4.10).

### 9.3 Java-isms observed, with evidence

| Anti-pattern | Evidence | Rust equivalent |
|---|---|---|
| Per-call scratch array of fixed huge size | `MinAreaTree.java:30`, `ShapeSearchTree.java:609`, `…45Degree.java:148`, `…90Degree.java:69` (`new ArrayStack<>(10000)`) | stack `SmallVec` / reusable `&mut Vec` |
| Eager string concatenation for filtered logs | `MazeSearchEngine.java:421-438, 560-588, 800-848, 907-963`; `ShapeSearchTree.java:651-666`; `MazeExpansionEngine.java:40-49, 102-111` | lazy `tracing` fields |
| Computation disguised as a getter | `ExpansionDoor.getShape()` `:43-47` (a convex intersection) | precomputed field |
| Per-object memo caches on transient objects | `Simplex.java:22-26` (`transient` corner/bbox caches on shapes that live one call) | cache on the long-lived owner |
| Boxed value types | `IntPoint`, `IntVector`, `IntDirection`, `FloatPoint`, `FloatLine`, `IntBox` all heap objects; `IntDirection.getVector()` `:36-38` allocates on **every** call | `#[derive(Copy)]` structs |
| Arbitrary-precision on a hot path | `Line.java:277-303`, `IntPoint.java:142-175`, `BigIntAux.java:42-62` | `i128` |
| Object-array `Arrays.sort` (TimSort) on already-sorted inputs | `Simplex.java:626`, `:43` | merge two sorted slices |
| Double-dispatch through abstract base classes | `Point.differenceBy`/`Vector.add`/`Vector.sideOf`/`Direction.compareTo` families (`IntPoint.java:96-117`, `IntVector.java:96-165`) | `enum` + `match` |
| `LinkedList` used as an indexed list | `MazeSearchEngine.java:559` + `:571-572` (`snapshot.get(doorIndex)` in a loop) | `SmallVec` + slice iteration |
| `LinkedList.sort` | `SortedRoomNeighbours.java:204-213`, `ShapeSearchTree.java:630` | `Vec::sort_unstable_by` |
| Ordered containers used only for determinism | `TreeSet<Leaf>` `MinAreaTree.java:26`; `TreeSet<SearchTreeObject>` `ShapeSearchTree.java:371`; `TreeSet<Item>` in every `getNormalContacts` | `Vec` + one `sort_unstable_by_key` at the end |
| Iterator object per poll | `MazeSearchEngine.java:327-329` | `BinaryHeap::pop` |
| Exceptions as control flow | `MazeSearchEngine.safeMazeSectionCount` `:252-258` | `Option` |
| Allocation to pass two scalars | `DestinationDistance.java:118` → `FloatPoint.boundingBox()` `FloatPoint.java:334-338` (2 `IntPoint` + 1 `IntBox`) | pass `(f64, f64)` |
| Validation logging inside a constructor | `IntPoint.java:19-29` (2 × `Math.abs` + `FRLogger.debug` per point) | `debug_assert!` |
| Reflection-based deep copy | `RoutingBoardUndoFacade.java:41-72` + `BasicBoard.java:1388-1400` (full R-tree rebuild on read) | arena `memcpy` / `Arc` COW / undo log |
| Eager task construction with heavyweight constructors | `OptimizeRouteTask.java:30` (deep copy **in the constructor**) + `BatchOptimizerMultiThreaded.java:251-260` (all N submitted at once, contradicting its own comment at `:249-250`) | bounded channel; build state inside `run()` |
| Full-collection scans as an API | `BoardConnectivityQueries.java:23-35` (`getConnectableItems`), `:38-…` (`connectableItemCount`), `RoutingBoard.java:1241-1250` | maintained per-net index |
| Per-call JMX round-trip | `AutorouteRuntimeMetrics.java:46` (`setThreadAllocatedMemoryEnabled(true)` on every read) | enable once |
| Hardcoded debug predicates left in shipped code | `AutorouteEngine.java:164`, `MazeFanoutDiagnostics.java:17`, `ShapeSearchTree45Degree.java:549-554` | delete |

---

## 10. One-paragraph summary for the implementer

Java Freerouting is **allocator-bound, not compute-bound**. A single spatial query costs ~80 KB of
zeroed, immediately-garbage array because v2 regressed v1.9's reusable traversal stack into a per-call
`new ArrayStack<>(10000)` at four sites; that one line explains a large fraction of the 3 TB. On top of
it sit three structural taxes: the entire expansion-room and drill-page database is destroyed and
rebuilt for every connection because `retainAutorouteDatabase` defaults to `false`; `ExpansionDoor.getShape()`
recomputes a convex intersection on every access and is called six-plus times per door per expansion;
and unguarded `FRLogger.trace` calls in the innermost maze loops force that geometry to run even when
logging is off. The optimizer's parallelism is paid for with a full Java-serialization board copy per
task, constructed eagerly for every item — the ~1 GB peak heap. `BigInteger` is real but narrow: it is
reachable from room completion only in any-angle mode, via `Simplex.corner() → Line.intersection()`,
and `i128` replaces it wholesale. In Rust, arena-allocate rooms/doors/drills/items with `u32` indices,
make all of `geometry.planar` `Copy` value types, hand every query a `&mut Scratch`, cache door shapes
and drill decompositions, persist the room database with dirty-rect invalidation, and maintain
connectivity incrementally with an endpoint index plus per-net union-find. Preserve, bit-exactly: the
frontier's four-level ordering with its `getId()` tie-breaks, the lazy-deletion pop loop, obstacle visit
order in `completeShape`, `SortedRoomNeighbour.compareTo`, the `sideOfIntersection` tolerance of 1.0,
the fanout insert-time gates, and every constant in §5.
