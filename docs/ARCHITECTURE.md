# fast-routing Architecture (v3 — post-review)

Goal: a Rust PCB autorouter that **matches Freerouting's routing capability and beats it
decisively on wall-clock time and memory**, on multi-core CPUs. No GPU work.

This document was adversarially reviewed twice (feasibility, parity); the review findings and
their evidence are folded in below. Where a claim is not yet measured it is marked **[to measure]**
with the experiment that will settle it.

## The reference

The reference is the **released artifact**, not a source tree:
`freerouting-2.3.0.jar` (Build-Revision `2d4de019aa89e9fa3dc1dc44e09bf509760cafc1`,
build date 2026-08-07). Pin it by sha256 in `bench/`.

Critical fact (verified by unpacking the jar and by live stack traces): the jar is a **hybrid**.
Its router core is the v1.9-style code (`autoroute/MazeSearchAlgo`, `AutorouteEngine`,
`BatchFanout`, `interactive/AutorouterAndRouteOptimizerThread`); the git-master
`autoroute/pipeline`, `autoroute/maze`, `board/facade` packages **do not exist in it**.
Its IO/DRC/scoring packages (`io/specctra`, `drc/`, `core/scoring`, `settings/`) ARE the v2 code.

Consequences:
- Router-core porting follows the **v19 sections** of `docs/upstream-study/` (the specs document
  both trees throughout). Claims labeled "v2-only router behavior" (pipeline stop rules, maze
  package refactor, bend costs, escape annulus values) must be **verified against the jar**
  (`javap -p -c`) or the source at the release revision before porting. A conformance pass over
  the four router specs is scheduled (M0.5).
- IO/DRC/scoring specs (`dsn-board-semantics.md`, `drc-connectivity.md`,
  `v2-vs-v19-and-scope.md` §3/§8) remain authoritative as written.
- CI carries a jar-package-audit check so the reference can never silently drift.

The reference **configuration** for all gates: `-mp 9999`, optimizer explicitly enabled
(`--router.optimizer.enabled=true`; note the jar's `getRunOptimizer()` null→false quirk means
default headless runs never optimize), fixed per-board wall budget stated in the bench manifest.
The original 300 s / `-mp 20` baseline is kept as historical data but is not the gate reference.

Java's own CLI provides three calibration oracles (verified working):
- `java -de f.dsn -drc report.json` → machine-readable violations of the **input** board
- `bench/tools/java_dump/` (DumpDsn) → a dump of Java's **resolved internal board model**.
  The older recipe `java -de f.dsn -do f.import.dsn --router.enabled=false …` does **not**
  work on the release jar: the v2.3.0 headless pipeline (`HeadlessBoardManager`) only writes
  SES, so `-do` never produces a DSN. DumpDsn walks the same load path
  (`loadFromSpecctraDsn` + `DsnWriter`) and is verified byte-identical to the checked-in
  `bench/golden/dumps/` oracles — see `bench/tools/java_dump/README.md`.
- deterministic outputs on boards where no wall budget binds (verified: 3 identical runs →
  byte-identical SES), so per-fixture references are stable once classified

## Measured baseline (provenance-split)

From the released jar, 46-fixture suite, 300 s cap, `-mp 20` (`bench/results/20260828-013025/`):

| metric | value |
|---|---|
| boards finished ≤ 300 s | 31 / 46 (14 timeouts, 1 crash-with-exit-0) |
| peak RSS | 0.4 – 11.1 GB (p50 ≈ 4 GB, JVM default heap sizing) |
| allocation rate while routing | ≈ 8.9 GB/s mean (per-stage telemetry) |
| optimizer stage ran | on 1 of 46 fixtures, 0.18 s total |
| stage split | fanout 499 s / routing 1344 s / optimize 0.2 s |
| **`T_java(complete)`** *(uncapped: 2 h cap, `-mp 20`, `bench/results/java-uncapped/`; fanout stage did **not** run in this campaign — not the same pipeline as the rows above)* | 11 of the 14 ex-timeout boards finish in **296 – 4675 s** each; suite now costed for 42/46 boards at **18 690.5 s = 5.19 h**. 1 board (Issue208) still hits the 2 h cap without finishing pass #1; 2 (Issue029, Issue107-bad) never route at all — they block on a modal `GuiManager.read_rules_file` confirmation dialog, so the earlier reading "two fixtures hang in DSN import" is superseded. |

Uncapped-row provenance and every caveat behind it:
`bench/analysis/measurement-campaign.md` (tables regenerable with
`python3 bench/analysis/campaign.py`; per-board values in
`bench/fixtures-manifest.json` under `fixtures[].uncapped`, where
`status == "completed"` is the only value whose wall time may be cited).
That write-up also carries the campaign's GC bound — forcing `SerialGC` costs
+6.0 % wall on the one clean, outcome-identical pair with allocation volume
unchanged — which is independent confirmation, from the collector side, of the
off-CPU argument in the next section.

From the **instrumented git-master source tree** (not the shipped jar — carries debug
instrumentation worth an estimated 15–35 % of time/bytes): the 3 TB-per-board and 5.9 GB
allocation figures quoted in `perf-forensics.md`. Use them for *where to look*, not as headroom
arithmetic.

## Why we expect to be faster — honest version

What the suite's own logs show: the Java router thread is off-CPU only **1.8 %** of its stage
time — stop-the-world GC is NOT the bottleneck; a 14-core machine absorbs the collector on other
cores. The ~40 % of process CPU burned by GC/JIT threads costs throughput, not the router's
critical path. Therefore "kill allocation → 10×" is **wrong** as stated.

The real levers, in expected order of impact **[to measure — experiments running]**:
1. **Recompute elimination**: per-connection room-database teardown (upstream's own retain flag
   exists as dead code — lab-jar experiment measures its value directly), uncached door shapes,
   full-board DRC ≥3×/pass, full ratsnest ≥6×/pass, O(N²) item scans, connectivity via repeated
   spatial queries.
2. **Constant factor**: Java object graphs → arena/SoA layouts; TreeSet queue → binary heap;
   BigInteger any-angle paths → i64/i128; allocation-free queries (the 80 KB-per-query stack in
   the shipped v2 IO/DRC paths); log-string construction in inner loops.
3. **Mutator-side allocation cost** (TLAB zeroing at ~9 GB/s): bounded, maybe 1.1–1.4×.
4. **Budget behavior**: upstream truncates pull-tight at 1000 ms and shove by wall time; a faster
   core does the same work within budget — quality headroom, not time.

Speed targets are therefore set per-milestone from measurements, not asserted. The measurement
campaign (M0.5): uncapped Java runs of the 14 timeout fixtures; `-XX:+UseSerialGC` bound on GC
cost; profiler stage split at DEBUG; lab-jar `retain_autoroute_database` experiment; lab-jar
expansion counters (budget calibration).

Known counter-evidence to respect: `Issue219`'s fanout runs 14 no-progress passes (~100 s) — an
upstream **algorithmic** non-convergence we must detect and exit, not just outrun. Two fixtures
hang in DSN import before routing. "The algorithm is good" has exceptions; the plan treats them
as first-class bugs to fix behind parity-safe stop rules.

## Parity policy

"能力对齐" means: on every fixture with a usable Java reference, our default mode ends with
**referee-judged `unrouted ≤ Java` and `added violations ≤ Java's added violations`** —
violations are counted as a **delta over the imported board** (26/46 fixtures ship pre-routed
copper whose violations are frozen: `(type route)` imports as USER_FIXED and is untouchable for
both routers; on 18/25 measured fixtures Java's router adds exactly 0). Import fidelity has its
own gates; the router is judged only on what it adds.

### Quirks that change routing results (replicate exactly; each with spec §ref)

- `Wiring.calcFixed`: `(type route)` → **USER_FIXED** (dsn-board-semantics §10.3) — decides
  rippability of all pre-existing copper; the single highest-value parity item
- `RouterSettings.applyBoardSpecificOptimizations`: per-layer trace costs derived from board
  aspect ratio, preferred-direction flip on signal layers, outer-layer surcharge
  (v2-vs-v19 §5) — the maze cost model is board-derived, not constant
- `copper_to_edge_clearance_um = 500` default: synthesized `board_edge` clearance class against
  every class, outline reassigned to it (v2-vs-v19 §3.1)
- `Structure.insertMissingPowerPlanes`: full-bbox ConductionArea with clearance class 0
- the `autoroute_settings` scope-order guard + `adjustPlaneAutorouteSettings` heuristic with its
  0.5·area / trace-free-layer thresholds (dsn-board-semantics §6.6, §11.6)
- convex-hull of non-convex pads (§8.1); `separateHoles` assignment semantics (§6.3)
- `reduceNetsOfRouteItems()` after load + the tie-pin DRC exemption that depends on it (§10.5)
- `ClearanceMatrix.getValue` out-of-range → 0 ("no clearance required")
- hard-coded `pullTightAccuracy = 500` ignoring the setting (v2-vs-v19 §18#14)
- coordinate scale `max(resolution,1)/10` integer division; the two half-width roundings;
  clearance round-up-to-even; compensation `(self+1)/2`; matrix read `[j][i]`
- conduction areas import as `is_obstacle = false`
- maze cost model, ripup toll formula (RNG seed = `ctrl.ripup_costs`, not `Random(0)` — that
  path is dead code), door gates, shove entry-point calculus, two-step offset with
  `c_offset_add = 1` — all verbatim, **after** jar conformance check
- item iteration order = descending semantic id (see ItemId below)
- DRC metric: octagon norm, integer-rounded, touching legal, dedup `(min_id, max_id, layer)`,
  compensation off, per-layer counting
- connectivity: exact integer-point equality; ratsnest node set = Pins ∪ non-tail Vias ∪
  ConductionAreas. Airline **count** must match; airline geometry and the Delaunay seed are
  explicitly NOT parity targets (only the count feeds the score, and upstream's own input order
  is HashSet-nondeterministic)
- pure-SMD via-cost ×0.1 and escape annulus values: port **only if present in the jar** (conformance
  pass decides; the two spec sources currently disagree 2.5–4.5 mm vs 0.5–3.0 mm)
- `ignore_net_classes` stays a no-op (it is GUI-only upstream; implementing it would diverge)

### IO round-trip fidelity (gated by import gates, not routing gates)

SES reserved-char set, rotation formatting, `(PN …)`, padstack-name dedup, quoted-string rules,
KiCad numeric-pin/rotate/oval-path traps, pad-center endpoint snapping (required for KiCad to
not show spurious unconnecteds), `library_out` completeness.

### Budgets (policy reversed after review)

Default mode keeps **upstream's wall-clock budget constants verbatim** (per-connection
`100 s·2^(pass−1)`, pull-tight 1000/2000 ms, fanout 10 s·pass, shove TimeLimit) — they define
the reference's stopping behavior, and on a faster core they simply bind less, which is the
honest way to convert speed into quality. An **effort ceiling** (expansion pops, shove visits,
pull-tight iterations) exists as a secondary cap only, default unbounded; bench records
`time_used`/`effort_used` and `connections_hitting_budget` per board so budget sensitivity is a
number, not a vibe. `--deterministic` mode replaces wall checks with effort-based virtual time
(calibrated ≥ upstream effort via lab-jar counters) so byte-reproducibility holds; it is the
debugging/G2.2 mode, not the gate mode.

Divergences live in `docs/PARITY-DIVERGENCES.md` from day one — including upstream bugs we fix
(v2 `Wiring.readViaScope` net-array regression: note it changes the input violation count on
fixtures with multi-subnet vias; the import gate carries both semantics).

## Crate architecture

```
fr-geometry        exact planar kernel: i64 fast path + checked promotion to i128, no heap
                   bignums on hot paths; IntOctagon complete; TileShape = enum{Box, Oct, Simplex};
                   Polyline stored as lines (corners derived)
fr-datastructures  arena ShapeTree: SoA nodes {8×i32 bounds, parent, kind}, caller-provided
                   result Vecs (zero alloc/query), STR bulk-load; epoch-scoped immutability:
                   readers pin an epoch, mutations advance it (RwLock or epoch scheme decided by
                   M1 microbench — the static tree is mutated on every routed connection, so
                   "read-only during routing" is false; the read-only window is one maze search)
fr-rules           ClearanceMatrix / NetClass / ViaRule / padstacks with the exact rounding
                   quirks; consumed by everything downstream
fr-board           item store: slotmap slots + a separate monotonic semantic ItemId (parity
                   iteration order is descending creation id; slot indices get reused and MUST
                   NOT leak into ordering); trace split/merge/normalize per upstream contract;
                   SearchTreeManager with compensated trees; incremental union-find connectivity;
                   three DISTINCT state mechanisms with honest costs:
                     1. transaction  = command log; snapshot O(1), rollback O(Δ) — shove/insert
                     2. board history = arena snapshot (memcpy of item+node Vecs), O(board) but
                        ~ms; restore must also restore tree state and preserves the room DB
                        interaction (restore invalidates per-item room caches — budgeted)
                     3. worker clone = CoW (Arc-shared immutable chunks), for portfolio/optimizer
                   M1 microbench gate: restore of a 30-pass-old board on the largest fixture < 50 ms
fr-io              DSN reader (lexer keeps raw token spelling; full KiCad trap list), SES writer
                   (pad-center snapping, reserved chars, integer coords), rules import,
                   golden-dump comparator (vs Java's -do dump)
fr-router          v19-semantics maze engine (room/door/drill arenas,
                   generation-stamped node state, lazy-deletion BinaryHeap, cached door shapes),
                   shove, locate/insert (ok-point trichotomy), pull-tight (worklists, region
                   sharding), fanout (with a no-progress detector for the Issue219 pathology),
                   batch loop (as shipped in the jar, verified by conformance pass).
                   The pre-rewrite `fr-autoroute` crate (incl. the CUDA stub) is deleted.
fr-check           the referee — DRC (octagon-norm, isObstacle table),
                   connectivity/ratsnest, v2 score. Calibrated against `java -drc` JSON on input
                   boards (multiset equality). Judges BOTH tools' outputs in bench.
                   The pre-rewrite `fr-drc` crate is deleted.
fr-core            pipeline: DSN → board → fanout/route/optimize → check → SES
fr-cli             CLI, Java-compatible flags where meaningful; exit codes preserved
```

Project-wide rules: no allocation in per-expansion/per-query/per-trial loops (scratch buffers in
worker contexts, bump arenas reset by generation); identity via u32 indices; f64 only where Java
uses floats, never in topology decisions; logging zero-cost when disabled; **no iteration over
default-hasher maps anywhere results can depend on order** (use ordered indices or seeded
hashers — determinism landmine flagged by review).

## Parallelism (restated honestly)

- **T1 portfolio/optimizer**: clone-eval-promote is upstream-proven *sound*, but under the
  historical baseline config the optimizer contributed 0.01 % of suite time — its parallel value
  depends on the re-pinned reference config actually exercising it. Portfolio racing buys
  quality-at-equal-wall-time, not speedup. Neither can carry a scaling gate alone.
- **T2 read-only fan-out** (referee DRC, ratsnest, room completion, region pull-tight): real but
  modest — Amdahl-capped at ~1.4× while the route pass (73 % of stage time) stays sequential,
  and incremental connectivity deletes much of what T2 would parallelize.
- **T3 concurrent connection routing** (`--parallel-connections N`, default 1): optimistic
  concurrent maze search on worker board snapshots with deterministic in-order commit and
  transactional DRC clearance oracle verification (`docs/upstream-study/parallel-connections.md`).
  Spreads independent connections across worker cores, achieving multi-core scaling (≥3× on
  spatially disjoint multi-net boards like Issue269 and Issue230) while preserving 0 added
  violations and deterministic outputs. Ripup and spatial conflicts gracefully fall back to
  serial routing.

## Benchmark discipline

`bench/run_bench.py` is the only source of performance truth. Both tools' quality numbers come
from **fr-check on their SES outputs** (self-reports are recorded but never gated on). The
fixture manifest classifies every board (`java_finished / java_timeout / java_crash /
java_degenerate`, `stable/unstable` across 3 Java runs, dedup key — the suite has ~39 independent
designs, not 46) and records coverage properties (40/46 are 2-layer, 0 use snap_angle ≠ 45°, 3
have power planes → a second-tier out-of-sample fixture set is mandatory, see ROADMAP G2.4).
Speed claims: wall-clock for the product headline (with JVM startup broken out), in-tool time for
algorithmic comparison, geomean **only over fixtures passing the quality gate in the same run**.
README carries only numbers reproducible by `python3 bench/run_bench.py`.
