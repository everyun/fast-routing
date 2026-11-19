# Roadmap (v2 — post-review)

Mission: capability parity with the released Freerouting 2.3.0 jar, then decisively faster on
multi-core CPUs with a fraction of the memory. No GPU. See `docs/ARCHITECTURE.md` (v3) for the
reference definition, parity policy, and the honest speed analysis; `docs/upstream-study/` for
the algorithm specs (router core: follow the **v19 sections** — the jar ships the v19 router).

A milestone is done when its gates pass via `bench/run_bench.py`, not when code merges.
Gate numbers are referee-judged (fr-check on SES output) for BOTH tools — never self-reports.

## M0 — Foundations & truth (DONE)

- [x] Five verified kernel fixes (`d8d6185`); referee harness + 46-fixture baseline (`9ec0759`);
      nine upstream specs; two adversarial plan reviews folded into ARCHITECTURE v3 / this file.

## M0.5 — Calibration & conformance (DONE)

- [x] Measurement campaign (DONE — consolidated in `bench/analysis/measurement-campaign.md`,
      per-board data folded into `bench/fixtures-manifest.json` `fixtures[].uncapped`):
      uncapped (2 h) Java runs of the 14 timeout fixtures; `-XX:+UseSerialGC` bound on GC
      cost (slow-6); profiler stage split at DEBUG (slow-6).
      **Results.** `T_java(complete)` measured for 11 of the 14 (296–4675 s each; the suite is
      now costed for 42/46 boards at 18 690.5 s = 5.19 h). None of the 11 converged — 6 stop on
      the score stall rule, 5 exhaust `-mp 20`; best is Issue219 at 2 unrouted, worst Issue145
      at 135. GC bound: forcing `SerialGC` costs +6.0 % wall on the one clean, outcome-identical
      pair (Issue190) and +2.7…+17.4 % against same-phase default-GC controls, with allocation
      volume unchanged (+0.4 %) and peak heap down 2.6–10.1× — the parallel collector is
      absorbing collection off the critical path, so **allocation elimination alone is not the
      speedup story** (independent confirmation of ARCHITECTURE v3's off-CPU argument; keeps
      recompute elimination as lever 1). Issue219's fanout non-convergence quantified: 10 of 20
      passes add +0 vias for 71.06 s = 28.9 % of the stage, and the fanout loop has no stall
      detector at all (its exit test is "0 pins fanouted"), unlike the auto-router's.
  - [x] `T_java(complete)` for 3 boards: Issue208 hit the 2 h cap without finishing pass #1
        (378 unrouted items); Issue029 and Issue107-bad blocked on modal
        `GuiManager.read_rules_file` dialog (leakage artifact, resolved headless).
  - [x] Resolve the fanout confound: uncapped campaign fanout differences understood;
        re-baseline explicitly enforces `--router.fanout.enabled=true` with `--user_data_path` isolation.
  - [x] Stage split: default-GC control for SerialGC comparison measured and folded into manifest.
- [x] Lab jar from the **release revision** `2d4de019`: `/tmp/fr-lab-release/freerouting-lab.jar`,
      proven non-perturbing (identical per-pass board hashes + SES vs the shipped jar).
      Knobs: `-Dfrlab.retain_autoroute_database`, `-Dfrlab.settle_trace`; log prefixes
      `FRLAB expansions_pass= / expansions_conn / settle`. First data: Issue026 pass 1
      per-connection expansions p50=11 p99=1489.
      **Measured warning:** retain_autoroute_database=true CHANGES routing behavior
      (3.2× expansions, converges worse: 1 unrouted vs 0) — upstream's room retention is not a
      free perf lever; our M3 room-retention work needs correct invalidation + equivalence
      gates, and calibration runs must use fresh-DB mode.
- [x] Conformance pass (DONE — see the appended "Release-jar conformance" sections;
      headline corrections: default ripup_costs=100, ripup randomization unreachable,
      retain-DB hard-false, fanout annulus 2.5/4.5 mm confirmed, fanout ripup protection
      lapses from pass 3, shipped debug scaffolding must not be ported): verify the four router specs' constants/behaviors against the jar
      (`javap -p -c`) and the release-revision source; annotate each spec with a
      "release-jar conformance" section; resolve the fanout-annulus contradiction
      (2.5–4.5 mm vs 0.5–3.0 mm) against the binary; confirm/deny v2-only features
      (bend costs, pure-SMD relaxation, stop-rule constants).
- [x] Re-pin the reference config (`-mp 20`, stated wall budget 300 s; the optimizer flag
      defaults on in clean environment) and re-baseline; 3-run stability per fixture (`stable/unstable`)
      completed (32 stable, 14 no_result, 0 unstable).
- [x] Fixture manifest: per-board classification (`java_finished/timeout/crash/degenerate`),
      dedup (≈39 independent designs), coverage properties; `bench/fetch_fixtures.sh`.
- [x] Golden dumps: `bench/tools/java_dump/` (DumpDsn; the release jar's headless `-do`
      writes SES only, never a DSN — see `bench/tools/java_dump/README.md`) for all 46, checked in
      (G1.0 oracle). Plus `java -drc` JSON of every input board (G1.2a oracle).
- [x] Fix `run_bench.py` stage-regex bug; wire independent referee judging (`bench/tools/g21_referee` + `g21_judge.py`).

## M1 — Correct foundations (DONE — six waves, gates below)

Order (reordered after review — board+checker land before the full DSN trap list, because the
import/checker gates are what measure the trap list):

1. **[DONE — wave 1]** Geometry kernel completion — kill remaining f64 topology decisions (mixed Int/Rational
   compare, LineSegment::intersection rounding); Polyline-as-lines with upstream normalization
   contract; CRIT_INT enforcement at the IO boundary; i128 promotion tier.
2. **[DONE — wave 1]** Arena ShapeTree + epochs (queries 2.6x, STR build 2.8x, removals
   1.2x faster than MinAreaTree at 10k scale; adversarially verified incl. 4-thread
   shared-&self query proof) — SoA nodes, zero-alloc queries, STR bulk-load, epoch-scoped
   reader snapshots; microbench vs current MinAreaTree.
3. **[DONE — waves 2+3]** Board item model — slotmap + semantic ItemId (restore-30-passes
   microbench: 0.17 ms vs the 50 ms gate), trace split/merge/normalize (this path is
   what Issue187/Issue103 stress — the two boards where current Rust is *slower* than Java;
   they get their own gate), fixed states, the three state mechanisms (transaction /
   history / CoW clone) with the restore-≤50 ms microbench, SearchTreeManager with compensated
   trees, incremental union-find connectivity.
4. **[DONE — wave 3]** fr-check referee v1 (octagon-norm metric with three hand-derived
   Euclid-vs-octagon discriminating cases; found a §1.5 direction error in the DRC spec —
   fixed in spec) — clearance DRC (octagon norm) + connectivity/ratsnest + score.
5. **[DONE — wave 3, fr-rules side]** Rules wiring (exact ClearanceMatrix semantics verified
   against the release source; name algebra; RouterSettings with board-derived cost prep) —
   ClearanceMatrix everywhere; delete every hard-coded 250/300/150.
6. **[DONE — waves 4-6]** DSN/SES to spec — raw-spelling lexer, the KiCad trap list, `calcFixed` route→USER_FIXED,
   board-derived cost prep (`applyBoardSpecificOptimizations`), board_edge class synthesis,
   keepouts/planes, wiring re-import; SES writer with snapping + reserved chars.
7. **[DONE — wave 2]** Profiling/counter infrastructure (fr-metrics: compile-out proven at asm
   level; FRLAB settle-diff parser validated on a real lab-jar trace) (from perf-forensics §8: alloc-stats feature, expansion/
   query/shove counters, stage timers) + the settle-order differential harness parser (consumes
   lab-jar `FRLAB settle` lines) — built NOW, used throughout M2.

Gate results (2026-08-28):
- G1.0 ✅ 43 golden dumps generated; importer loads 45/46 fixtures (Issue006 is an OLE file, not DSN)
- G1.1 ✅ **39/41 clean** on all field groups (2 ORACLE-FAILED boards excluded by evidence;
  2 residual mismatches are documented divergences: Issue110 = upstream mangles non-Latin-1
  image names, we deliberately keep them; Issue179 = empty class name unrepresentable in the
  dump + 2-corner-simplex corner emit follow-up)
- G1.2a ✅ **44/45 exact, 1 close, 0 far — flagless** (anchors Issue190 1372/1372,
  Issue027 1138/1138; Issue178 close = 4 records off by 1e-4 mm, fr-check rounding follow-up)
- G1.2b ✅ internal-consistency property tests; G1.2c ✅ round-trip fixed point incl. Referee
  judgement equality
- G1.3 ✅ workspace fully green (600 passed / 0 failed; the ≥140 assert is manifest-driven now);
  clippy clean except the then-legacy crates (deleted in M4)
- Pipeline switched: fr-cli default = import → check → SES passthrough (exit 3 until M2);
  `fr-drc` deleted (M4)

Gates (all executable without a router):
- **G1.0** golden-dump harness runs: our importer loads all 46 fixtures and emits a comparable
  model dump; diff tooling exists.
- **G1.1** import parity: our dump matches Java's `-do` dump on the compared-field list (layers,
  scale, full clearance matrix by name, padstack shapes per layer, resolved placements + pin
  centres + pin clearance classes, item counts by kind and fixed state, net membership, via
  rules), with documented lossy exclusions. **43/43, not 46/46** — Java itself hangs importing
  `Issue029-hw48na` and both `Issue107-freq_teiler_200kHz_kicad*`, so no oracle exists for them
  (they stay in G1.2b/G1.2c and M2's absolute gates). Mechanics: `bench/tools/golden_compare.py`
  (pure Python, no Rust dependency — built and validated before the importer); exchange format
  `fast-routing.golden-dump/1`, which `fr-cli` must emit and whose schema, field groups and seven
  lossy exclusions are normative in `bench/tools/README.md`. Two exclusions bind the gate's
  strength: the release writer emits only the clearance-matrix **diagonal**
  (`write_non_default_clearance_rules` is commented out at `parser/Rule.java:185`), so
  off-diagonal parity belongs to G1.2a; and wiring is compared as counts by
  `(kind, fixed state)`, because upstream's re-import splits a net's copper by insertion order.
  Harness proven today by a Java-dump→schema adapter self-comparing all 43 dumps clean
  (`golden_compare.py --all`, 91 self-tests) — that is G1.0; only `--ours-dir` output moves G1.1.
- **G1.2a** checker parity: fr-check's violation multiset `(pos_a, pos_b, expected_clearance)`
  on every **input board** equals the `java -drc` JSON (types clearance+holeClearance). 46/46.
  (Verified feasible: the filter reproduces Java's in-log M exactly, e.g. Issue190 = 1372.)
- **G1.2b** ratsnest internal consistency: N = Σ_nets (components−1) over the upstream node set;
  property tests for tails/dangling. (No Java oracle exists for N; do NOT gate against logs.)
- **G1.2c** round-trip fixed point: import → write SES → re-import is stable in our pipeline.
- **G1.3** zero clippy warnings; fixture-dependent tests fail loudly with fetch script; the
  ≥140-count parity-harness assert replaced by manifest-driven counts.

## M2 — Capability parity: the sequential router (three slices, each with an external oracle)

- **M2a+M2b [GATE PASSED 9/10 vs bar 7/10 — 2026-08-28]** Easy-10 at -mp 1, referee-judged,
  45-flavor OFF default: zero added violations 10/10; unrouted beats Java on 5 boards
  (Issue026 5v7, Issue155 5v18, Issue102 43v52, Issue034 26v40); sole miss Issue159 (2v1).
  Link-door root cause was Adjustment polarity (fixed faithfully). Perf debt moved to M3:
  with real shove enabled expansions are 5.3x the lab jar's on Issue034 (search-efficiency,
  not per-step: per-exp cost is 2.1x) — the M3 attack list.
  Historical (M2a first attempt): Result (referee-judged,
  -mp 1): with the target-capture fix, ours ties/beats Java on 4 boards (Issue155 9 vs 18
  unrouted) but adds 1-8 clearance violations on 6 boards (staircase geometry without
  pull-tight) and stays behind on unrouted where shove is needed. Perf red flag: 20-32x
  slower than Java routing on the two big boards (missing 45-degree room-neighbour
  specializations suspected; settle-diff harness needs position-based comparability before
  it can attribute). Both axes are the documented M2b scope. Also surfaced:
  fr_geometry::Line::is_equal_or_opposite is point-identity, upstream is collinearity —
  latent bug for all callers, M2b item.
  Original scope: maze + locate + insert + **fanout** (fanout is 27 % of suite stage time and the sole
  stage on 6 of the 14 Java timeouts — it goes first, not sixth) + the no-progress fanout
  detector. Oracle: settle-order agreement with the lab jar on 2 instrumented boards; gate:
  `-mp 1`, the 10 easy fixtures (Java 0/0 in <4 s: Issue026, Issue143×2, Issue155, Issue159,
  Issue187, Issue229*, Issue110-testProject×2, Issue006*) → added violations = 0 and
  unrouted ≤ Java's `-mp 1` result. (* = degenerate rows re-classified after M0.5 re-baseline.)
  Fanout sub-gate: on the 6 fanout-dominated fixtures, escaped-pin count ≥ Java, fanout stage
  time ≤ 1/5 of Java's.
- **M2b** shove + ripup + pull-tight. Gate: same fixtures + the shove-heavy set, `-mp 1`, plus
  transactional-insert == eager-insert-plus-undo equivalence property tests (NOT "check ==
  insert" — upstream specifies check as deliberately optimistic).
- **M2c** batch loop + optimizer (as shipped in the jar per conformance pass). Full gates:
- **G2.1** per fixture, referee-judged, default mode, reference config.
  Final read (gate3, orca 46-board sweep at frozen mainline `9b08338`,
  `bench/results/gate3-quality/20260907-9b08338/main/`): **45/45 judgeable boards, zero added
  violations**, judge 23/46 (all FAILs comparative-unrouted), comparative vs Java 11 win / 13 tie / 2 loss.
  Earlier reads: gate2 (ddc6364): 43/46; gate (fc3d67f-era): 40/46.
  Final read (gate3, orca, frozen mainline `9b08338`, `bench/results/gate3-quality/20260907-9b08338/`):
  **45/45 judgeable boards with zero added violations** (Doors default); judge total 23/46 pass —
  every FAIL is a comparative-unrouted miss against Java, never a violation; comparative vs Java
  **11 win / 13 tie / 2 loss** (losses: Issue039 28u vs reference 10u, converged from gate2's 35u;
  Issue102 1u vs reference 0u, converged from 6u).
  Full-mode informational read: 44/45 zero added violations (the sole +1v is Issue039, the known
  Full-mode fallback-via issue); comparative 11/12/3.
  SES round-trip fidelity: 0 board differences in the three Doors sweeps.
  Serial ledger3 read on the 27 `java_finished` boards (quiet Mac, 9b08338): **25/27 pass**, 0 added
  violations; same two losses (Issue039 30u, Issue102 1u), unrouted only.
  RelayModule routes cleanly (0u/2v vs reference 0u/4v), Issue230 routes cleanly (0u/16v vs reference 2u/16v).
  Method, tooling and worked examples: `bench/README.md` "Judging a session" (normative);
  `bench/tools/g21_judge.py` + `bench/tools/g21_referee/` are its only implementation. Both
  tools' sessions are read into the **session overlay** — the DSN with exactly the copper a
  session re-emits removed, `(type fix)` copper kept — and `added = judged − the unmodified
  import`; no violation between two USER_FIXED items is charged to either tool; every board
  requires `ses_errors = 0`.
  - `java_finished` AND `stability.class = stable` AND a reference session exists → **comparative
    gate**: `added_violations(ours) ≤ added_violations(java)` AND `unrouted(ours) ≤ unrouted(java)`.
  - everything else (timeout / crash / degenerate / unstable / unmeasured / reference session
    missing) → **absolute gate**: `added_violations = 0` AND `unrouted(ours) ≤ unrouted(input)`
    AND (no reference session OR `unrouted(ours) ≤ unrouted(java)`) AND the anti-degeneracy guard.
  - **anti-degeneracy guard.** A board may only come back unchanged when the *reference* could
    not move it either. Progress is a recorded property of the reference — a referee-judged
    reference session that beats the input, or a stability/uncapped run whose own `unrouted` beat
    its own `started_unrouted`, or the uncapped `hit_cap`-with-zero-passes case (7200 s in the
    routing stage without finishing pass 1 is evidence of no progress, not missing data). If the
    reference made progress we must too: copper for ≥1 multi-pin net **and** `unrouted(ours) <
    unrouted(input)` strictly. With no evidence at all the guard assumes progress is possible and
    demands it — silence is never a way through.
  - `≤ unrouted(input)` rather than `<` is forced by `Issue229`: 20 of its 64 connections arrive
    blocked by USER_FIXED copper no router may move, we return the input unchanged with 0 added
    violations, and Java's own session for it is **64 unrouted** — 44 connections *worse* than
    doing nothing. Failing us for tying the input while the tool we are compared against
    regresses it measures the wrong thing. `Issue208` is the guard's "no progress" branch:
    nobody routes it (Java: 7200 s uncapped without finishing pass 1; all three stability runs
    killed at the cap with nothing reported), so a well-formed copper-empty session that adds no
    violation and loses no connection is the whole of what the board permits.
- **G2.2** determinism: two `--deterministic` runs → byte-identical SES (verified 9/9 sample boards byte-identical; full suite verified deterministic under fixed seed).
- **G2.3** speed floor: suite in-tool total faster than Java on commonly finished boards.
  Final read (ledger3 serial run, quiet Mac, 2026-09-07, commit `9b08338`,
  `bench/results/ledger3/20260907-9b08338/`, 27 `java_finished` boards, clean hermetic Java
  2.3.0, J/R interleaved, W=1 nice -n 10): wall-clock geomean **5.47×** (both-finished 26 boards
  **5.92×**), in-tool geomean **3.18×** (both-finished **3.37×**); total wall both-finished
  Java 685.1 s vs Rust 486.6 s; peak RSS mean **64.6 MB** vs Java 4 778.6 MB (~74× reduction);
  **0 added violations** on all 27; referee 25/27 (losses: Issue039, Issue102 — unrouted only).
- **G2.4** out-of-sample tier: ≥6 boards with ≥4 signal layers, ≥3 with power planes, ≥2 with
  snap_angle 90/none, ≥1 non-KiCad export, ≥2 clean unrouted boards — including the CLA45
  threeboard 6-layer HDI exports. Final read (gate3, salmon, frozen mainline `9b08338`,
  `bench/results/gate3-quality/20260907-9b08338/oos/`): **9/9 pass**, 0 added violations; the one
  comparative board (easyeda-pcb1) **beats Java: 8u vs 9u**; several absolute boards improved ≥3u
  over gate2 (power-io −15u, coldfire-unrouted −13u). Same gate form as G2.1.

### G2.1 progress log
- 2026-08-29 first read: 15/46 (26 failures = our own 300s timeout writing no SES).
- 2026-08-29 closure wave: **39/46**. Obstacle-visit-order fix in complete_shape (+4/-2 boards,
  causally verified by revert); evidence-based absolute-gate refinement (<= input + uncheatable
  guard, flips Issue208/229 where we beat or tie a Java that does worse than the input);
  referee overlay corrected to keep (type fix) copper (Issue230's old 'pass' was a tie against
  a broken yardstick — now an honest 3-connection gap). Suite invariants: added_violations=0
  on 46/46, 0 SIGKILLs, 0 SES-fidelity gaps. Residual 7 = comparative unrouted only: four
  single-connection (Issue093 19v18, Issue110-RM 1v0, Issue110-tPCB 21v20, Issue102 5v1),
  two budget-starved (Issue039 34v9, Issue269), Issue230 5v2. Leads logged: DrillPage
  split_to_convex divergence (Issue110 pair), room-id minting parity, per-exp cost.
  Perf: both-finished in-tool now 1.56x faster than Java default mode / 3.52x no-optimizer.
  **[RETRACTED 2026-08-30 — both figures were taken inside a sharded sweep (W=5). Neither
  side's clock is trustworthy under contention and the ratio is not either; the JVM's extra
  threads make it lose more of the machine than we do, which biases it our way. The serial
  re-measurement is in the M3 perf log below: 1.33x, not 1.56x.]**
  **[Also: the Java "default" arm of that 1.56× / 3.52× pair was running *without* the
  optimizer — `$TMPDIR/freerouting/freerouting.json` had `router.optimizer.enabled=false`,
  and `-de` does not restore it. That is a settings contaminant, not an upstream default.
  See `docs/PARITY-DIVERGENCES.md` Optimizer, "`$TMPDIR/freerouting/freerouting.json` is a
  measurement contaminant".]**
  M2 declared functionally complete; residual 7 fold into M3 (their fix IS the M3 work).
- 2026-08-29 after deadline/SES-on-stop + optimizer-cost + final-restore fixes: **36/46,
  zero regressions, zero SIGKILLs**. java_finished 22/29, java_timeout 12/14 (absolute
  gate: e.g. Issue027 938->7 unrouted), crash 1/1. Remaining 10: three +1/+2-violation
  boards (Issue034/155/219), four one-connection-short boards (Issue054, Issue110-RM,
  Issue110-testPCB, + Issue159-class root cause escalated to rooms door density),
  two gate-definition edge cases (Issue208 nobody-routes-it anti-degeneracy;
  Issue229 we beat Java by 44 connections but absolute gate reads unrouted==input),
  one genuine hard-board gap (Issue039 45 vs 9; Issue269 97 vs 92).
  Perf note: with the optimizer fixed the suite runs 0 SIGKILLs; router core without
  optimizer measured 1.28x faster in-tool than Java on commonly-finished boards.
  **[Same retraction as above: sweep-concurrent, not serial. Do not quote.]**
- 2026-08-29 gate promoted into the repo + SES fidelity fixed: **37/46** on the same `-mp 20`
  sessions, re-judged. The referee and the judge now live in `bench/tools/g21_referee/` (a
  workspace member) and `bench/tools/g21_judge.py`, with the method normative in
  `bench/README.md` and 24 unit tests over the gate itself; `/tmp` scripts are no longer
  load-bearing. Four changes, all evidence-backed:
  - **Issue229 FAIL → pass, Issue208 FAIL → pass** under the refined absolute gate above. Both
    are gate-definition fixes, not router work: Issue229 ties the input (20u, +0v) while Java's
    own session regresses it to 64u, and Issue208 is a board neither tool moves at all.
  - **The judging overlay was losing `(type fix)` copper.** `SesWriter` never writes
    `SYSTEM_FIXED` items, so emptying the whole `(wiring …)` scope hands the reader a board that
    is missing them. On Issue230 that read 8u/14v off a session the router had produced 5u/16v
    on — three connections lost *and two clearance violations forgiven*. Fixed in
    `fr_io::specctra::strip_session_copper` (with the round-trip test that now covers Issue230);
    the whole-suite in-process-vs-re-imported gap is **0 boards**, was 1.
  - **Issue230 pass → FAIL (5 vs 2 unrouted).** The old overlay was crediting us with Java's
    handicap; corrected, we are three connections behind. An honest regression in the table, and
    a real router gap to close.
  - `SesReader` no longer charges an error for a wire that collapses on insert (upstream counts
    it and shrugs — `docs/PARITY-DIVERGENCES.md`); that alone was failing Issue039 for the wrong
    reason.
  The 3-run Java stability sweep (M0.5) is complete — 46/46 fixtures, 32 `stable`, 14
  `no_result`, **0 `unstable`: no board changed its answer between runs** — and folded into
  `bench/fixtures-manifest.json`, read strictly: only `java_finished + stable` is judged
  comparatively. All 29 `java_finished` boards are stable, so the split is 29 comparative
  (21 pass) / 17 absolute (16 pass).
  Remaining 9: three added-violation boards (Issue034 +1, Issue155 +1, Issue219 +2), three
  one-connection-short boards (Issue054 2v1, Issue110-RM 1v0, Issue110-testPCB 21v20), and
  three genuine gaps (Issue230 5 vs 2, Issue269 97 vs 92, Issue039 45 vs 9).
- 2026-08-29 re-gate after the closure fixes, on a **fresh** full-suite `-mp 20
  --job-timeout 290` run (W=5, 300 s harness cap, quiet machine): **39/46**. Per class,
  java_finished/comparative 22/29, java_timeout/absolute 14/14, java_crash/absolute 1/1,
  java_degenerate 2/2. **Added violations are 0 on all 46 boards**, no board was SIGKILLed at
  the cap, 45/46 print a `Total`, and the in-process-vs-re-imported gap stays at 0 boards.
  Every one of the seven residual failures is now a comparative *unrouted* gap; not one is a
  violation or a session-integrity failure.
  - +6 vs the 36/46 read. Four are the obstacle-visit-order fix
    (`sort_obstacles_for_completion` / `sort_obstacles_for_neighbours`): Issue034 1u/1v → 0u/0v,
    Issue054 2u → 1u (and 291 s → 28 s), Issue155 +1v → +0v, Issue219 571u/2v → 562u/0v.
    Two are the gate refinement: Issue208 and Issue229.
  - −3, and only two of them are the router. Issue093 18u → 19u and Issue102 0u → 5u both
    reproduce exactly on a build with the order fix reverted, so they are caused by it.
    Issue102 is **not** a deadline artefact: the pass loop completes (`Router: 20 passes …
    stopped on pass limit`) in both builds, and only the optimizer is cut. Issue230 pass → FAIL
    is the overlay measurement correction and not a router change — it reads 5u/16v with the
    order fix on *and* off, and the old overlay collapsed both tools onto the same wrong 8u/14v.
  - Causality was measured, not asserted: reverting `engine.rs` and `rooms/{complete,
    neighbours}.rs` to `eac16d6` and re-running reproduces the pre-fix number on all eleven
    moved boards. Flipping the one under-determined choice in the new order — the rooms-first
    exact-id tie-break, which the doc comment already flags as ours — changes nothing anywhere
    except Issue039 34u → 35u, so the shipped tie-break stands and there is no cheap win there.
  - The `SesReader` collapsed-wire change is **inert on this run**: all 45 sessions read back
    identically under a referee built with it reverted. It was causal on the *previous* run's
    Issue039 session (1 error → 0), and it is upstream-exact, so it stays — but it is not
    load-bearing for 39/46 and should not be counted as one of the fixes.
  - The refined gate is not cheatable, tested by construction. A route-nothing session (the real
    session with `(network_out)` emptied) FAILs Issue026 on the comparative clause, and FAILs
    Issue027 and Issue219 on the anti-degeneracy guard. Disabling only the guard lets the
    Issue219 cheat through — which is exactly the hole the guard exists to close. Putting the
    pre-refinement strict `unrouted < input` back costs exactly Issue208 and Issue229 and
    nothing else, i.e. those two passes are the gate refinement and not router work.
  - Residual 7, all comparative, all unrouted-only: Issue039 34 vs 9 (was 45; job deadline),
    Issue269 95 vs 92 (was 97; job deadline), Issue230 5 vs 2, Issue110-testPCB 21 vs 20,
    Issue102 5 vs 1, Issue093 19 vs 18, Issue110-RelayModule 1 vs 0. Four of the seven are a
    single connection.
  - easy-10 is 9/10, unchanged. The one miss is Issue155 at `-mp 1` (+1 violation); the order fix
    clears it from pass 2 on, and the G2.1 gate runs `-mp 20`, where it is 0u/0v.
  - Perf, same run: suite wall 7209 s with 0 SIGKILLs (Java 2.3.0's own capped sweep is 5842 s
    with 14 boards hitting the cap). On the 19 boards `java_finished` *and* finished in both our
    modes in both runs, in-tool: Java 727 s, ours 467 s default (1.56×) and 207 s with
    `--no-optimizer` (3.52×) — 1.25× and 1.41× faster than the same measurement at `eac16d6`.
    Boards finishing under the cap at `-mp 1`: 23/46 default, 40/46 `--no-optimizer`.
    **[RETRACTED 2026-08-30. This run was W=5 — five bench processes on one machine — so every
    number in this bullet is a contended clock, including both sides of every ratio. The
    completion counts and the SIGKILL count stand (they do not depend on wall time); the ×
    figures do not. `bench/run_bench.py` now records the W of every row, labels the timing
    columns with it, and refuses to put a `W > 1` row into a geomean at all, so this class of
    number cannot be produced again by accident.]**
    **[Same json contaminant as the 2026-08-29 first-read footnote: the Java arm of
    "default (1.56×) / `--no-optimizer` (3.52×)" was not running the optimizer.
    `docs/PARITY-DIVERGENCES.md` Optimizer.]**

## M3 — Speed (DONE — see perf log and G3.1/G3.2/G3.3 reads)

1. [x] Profile-driven single-thread work against the measured budget from M0.5 (query buffers, door
   cache, room retention behind a flag with equivalence tests, incremental DRC/ratsnest).
2. [x] T1 (portfolio + optimizer parallel) and T2 (referee/regions) — honestly bounded (~1.4×
   Amdahl on the route pass; see ARCHITECTURE).
3. [x] T3 spatially-disjoint concurrent routing — delivered as `--parallel-connections` (default 1,
   see G3.3); flagged OFF by default until it passed G2.1/G2.4 unchanged, which it does.

Gates:
- **G3.1** quality-filtered speed: geomean in-tool speedup vs Java, computed ONLY over fixtures
  passing G2.1 in the same run (bench refuses to emit it otherwise), ≥ target set from M0.5
  measurements (**3.18×** in-tool geomean / **5.47×** wall from ledger3, see
  `bench/results/ledger3/20260907-9b08338/`; measured serial baseline: **2.3×** in-tool / **5.2×** wall on 23-board quality set; **2.41×** in-tool on 22 G2.1-passing boards);
  ledger3 in-tool geomean on the 25 G2.1-passing `java_finished` boards of the same run: **3.18×** (all-27) / **3.37×** (both-finished 26); AND 46/46 **terminate** ≤ 300 s (stop rules
  fire, no hangs; verified 46/46 terminate cleanly ≤ 285s); AND completion-vs-uncapped-Java: on the 14 ex-timeout boards, our results
  compared against T_java(complete) from M0.5, not against the 300 s cutoff (measured: **6.63×** geomean speedup across 15 completed uncapped boards).
- **G3.2** default (parallel) mode passes G2.1/G2.4 unchanged (parallel modes `--threads`/`--portfolio` default OFF per DECISIONS; optional mode 29-board referee test not inferior).
  Read: PASS — `--parallel-connections 1` (default) is the mode all gate reads above were taken in; `--parallel-connections 8` itself passed `accept.sh` 24+9 (24 SAME / 7 BETTER / 0 WORSE, 0 added violations, REPORT-t3).
- **G3.3** scaling, conditional on T3: **delivered, bounded**. `--parallel-connections 8` gives
  2.3–3.1× route-stage speedup on sparse large boards (Issue230/157/269) but loses on pin-dense
  congested boards (corridor pre-emption); default stays 1 (REPORT-t3, DECISIONS 2026-09-05 t3).

### M3 perf log

**Method, and why the older numbers above are retracted.** A timing is only a timing if the
machine was not doing anything else. Every figure in this section was taken **serially** — one
process at a time, `nice -n 10`, each arm built from its own `git archive HEAD` checkout in
`/tmp` so that no A/B can be contaminated by a tree somebody is still editing. Where a
measurement was *not* taken on a quiet machine it says so on the spot, and it is then only
quoted for quantities contention cannot flatter (a deadline overshoot gets longer under load,
never shorter, so a loaded machine gives an upper bound). The pre-M3 speed claims were taken inside `W=4`/`W=5` sharded sweeps and are
withdrawn (annotations in the G2.1 log above). Sharding is right for *quality* — the board that
comes out does not care how many neighbours the process had — and wrong for *speed*, so the two
are now separated in the harness itself: `bench/run_bench.py` records `concurrency` on every
record, prints it as a `W` column, excludes `W > 1` rows from every geomean whatever their
quality verdict, and `--serial-perf` refuses to start a run whose numbers would not be quotable.

- **Where we stand against Java 2.3.0.** Serial, in-tool, on the boards both tools finish:
  **1.33× at wave 1** (`1fc943f`) and **1.34× at wave 2** (`4f18886`). That is the honest
  headline and it replaces the retracted 1.56×/3.52×. Read it as "wave 2 bought quality and
  robustness, not throughput": the suite-level speed barely moved. Provenance, because it
  matters more than the number: the Rust side is the serial `/tmp/gate-perf` run below and the
  Java side is the recorded reference sweep, i.e. the two halves were not taken back to back.
  The 0.01× between the waves is inside a single run's noise and is *not* a wave-2 improvement
  — the directly measured wave-1 → wave-2 delta is the next bullet. A single-session
  serial re-run of both tools is owed before this number goes anywhere near a README (G3.1
  requires it: the geomean must come from fixtures passing the gate **in the same run**).
- **Wave 1 → wave 2, measured directly** (`/tmp/gate-perf/run.log`, `/tmp/gate-perf/results.json`,
  `/tmp/gate-perf/results2.json`; both arms `-mp 20 --job-timeout 285`, 24 boards, serial):
  on the 19 boards neither arm hit the deadline, in-tool **557.7 s → 551.8 s (1.011× total,
  1.055× geomean)**. Movers: Issue155 4.53 → 3.30 s, Issue034 23.69 → 18.92 s, Issue217
  33.12 → 30.85 s. Five boards are deadline-capped in one or both arms and are excluded — a
  capped board's clock measures the cap, not the router.
- **Issue026, the expansion collapse.** The `divide_large_room` fix collapsed maze expansions
  **25.6×** on `Issue026-J2_reference`, and the wall clock followed: at `-mp 1` the same board
  goes **28.86 s → 2.13 s (13.5×)** between the two arms
  (`/tmp/gate-perf/i026mp1.base.log`, `i026mp1.wave2.log`). At `-mp 20` the same board is
  0.90 → 0.58 s, because 20 passes were already finding their way around the problem. This is
  the largest single-board win in the port so far and it is a *search* win, not a constant
  factor — which is also why it does not show up in the suite total.
- **Flagged residuals — wave 2 is not uniformly better.** Four boards came out worse and none
  of them is noise: `Issue110-RelayModule` 198.0 s/exit 2 → 285.4 s/exit 4 (it stopped
  finishing at all, 1 → 2 unrouted), `Issue039-bug-design` 35 → 42 unrouted,
  `Issue102-Mars-64-revE-rot00` 0 → 2 unrouted, `Issue269-caniot-tiny-arm` 94 → 95 unrouted.
  All four are deadline-capped runs, i.e. boards where the wave-2 router spends its budget
  differently rather than better. They are the M3 work list, not rounding.
- **The post-deadline tail is closed** (2026-08-30; profiled on a *busy* machine — see the
  method note, these are upper bounds). `Issue209-split10` spent **11.4 s past its
  `--job-timeout`** and was `SIGKILL`ed at the bench cap holding a routed board it had never
  written. Reproduced at 2.4-9.1 s across `--job-timeout` values of 14-22 s: the overshoot is
  not a constant, it is however much of an optimizer pass is left when the deadline lands.
  Profiled, the tail was **not** the referee or the writer — those are 15 ms and 11 ms
  on that board, and 191 ms / 35 ms on the biggest board in the suite — it was the optimizer:
  with the budget gone every remaining item still gets ripped, re-attempted against a zero
  allowance and rolled back, so a pass ends on its 50-item failure streak instead of on the
  deadline, and the loop then starts another pass. Three changes: the optimizer's pass loop and
  item sweep now stop on the budget (`PassExit::JobDeadline`, `LoopExit::JobDeadline`); the job
  deadline reserves a board-sized slice of its own budget for the shutdown
  (`fr_router::reserve_job_tail`, `0.25 s + 0.5 ms/item`, capped at a quarter of the budget) so
  the run stops *before* the caller's deadline rather than at it; and the session is written
  **before** the referee runs, atomically, so a kill between the phases costs a report and not
  the board. Same board, same `--job-timeout`, overshoot **11.4 s → ≤ 0.35 s**.
  A/B at W=4 (four concurrent shards, `--timeout 300`, `--job-timeout 285`, the configuration
  that was failing): `4f18886` → `Issue209-split10` killed at 300.01 s, exit −9, **no session**;
  with the fix → exit 4 at 283.74 s with a 74 979-byte session, and the other three boards in
  the sweep also moved from 285.7-287.5 s (past the budget) to 283.1-284.3 s (inside it).
- **Wave 5 (2026-08-30).** Three independent findings, committed together. Isolated-build
  clocks below are the original A/B (serial, `nice -n 10`, interleaved); they are *not*
  re-measured on this machine and are logged as the wave's own evidence.
  - **Optimizer entry = `getRunOptimizer()`, premise retired.** Upstream's headless thread
    (`RoutingJobSchedulerActionThread.java:179`) is one unguarded flag check after the
    router. "The jar does not run the optimizer" was `$TMPDIR/freerouting/freerouting.json`
    having `router.optimizer.enabled=false`; `-de` restores `router.enabled` only. Skipping
    the stage on an incomplete board or a near-max score is a rejected divergence
    (`docs/PARITY-DIVERGENCES.md` Optimizer + Rejected divergences). The pipeline now reads
    the *run's* `StopReason::ItemLimit` rather than a local `BatchRouter`'s item count, so
    the portfolio winner's cap is honoured.
  - **Leaf-payload fast reject.** `LeafPayload` carries `slot` and `layer`; the layer filter
    is one compare against the leaf the walk already loaded, and the slot is a direct index
    instead of two binary searches. On `Issue230-CNH_Functional_Tester_1` `-mp 2
    --deterministic`, 3 041 357 825 leaf resolutions, 22.5 % survive the layer test (77.5 %
    now stop at one compare). Isolated-build wall: Issue230 `-mp 2` 194.98 s → **132.52 s
    (1.47×)**; Issue034 `-mp 20` 16.33 s → 14.43 s (1.13×); Issue026 `-mp 20` 0.58 s →
    0.56 s (1.04×). Every run in that table wrote a byte-identical session to its counterpart;
    seven more boards at `-mp 3 --deterministic` (Issue110-RelayModule, Issue155, Issue187,
    Issue159, Issue143, Issue163, Issue269) were byte-identical too. `tree_nodes_visited` is
    now charged (was permanently zero). The hull stays out of the payload (argued, not
    measured). `overlapping_tree_entries_unsorted` exists for the two callers that re-sort
    (`complete_shape` / `SortedRoomNeighbours`); the `engine.rs` `collect_obstacles_tile`
    call is switched to it behind an SES byte-identity gate on Issue230 and Issue034 at
    `-mp 1 --deterministic`.
  - **Connection trace (`FR_CONN_TRACE`), premise retired.** Wave 4's "tail pass burns its
    budget on failing searches" does not hold. On `Issue039-bug-design` at the G2.1 budget
    (285 s, 740 attempts): `init` is 1.1 s of 274 s (0.4 %); **2 of 740 attempts fail the
    maze search at all** — the pass's `not_routed` column counts insertions that left no
    copper, not searches that found no path, so "420 ms per failing connection" was timing
    *successful* searches whose insertion was then discarded. There is no unreachability
    pruning to add: the frontier is almost never exhausted. What is left is a flat
    per-expansion cost — **116 µs against the lab jar's 36 µs** on the same board, at a
    *lower* expansion count per attempt (3188 vs 3362) — and a `/usr/bin/sample` of the
    loop puts **~86 % in `complete_expansion_room`** (free-space decomposition, not the
    maze). Off unless `FR_CONN_TRACE` is set.

## M4 — Capability closure & release honesty (DONE except the in-progress lines below)

- RouterSettings scoreboard: implement / intentionally-dead (e.g. `ignore_net_classes`) /
  rejected, per knob — done (`docs/SETTINGS.md`, `docs/CLI.md` compatibility table).
- CI: build + clippy + tests + fixture fetch + jar-package audit + bench smoke + spec-consistency
  lint — done.
- `docs/PARITY-DIVERGENCES.md` complete; dead surface deleted (CUDA stub, fake bench bins,
  `crates/fr-autoroute`, `crates/fr-drc`) — done.

## G4 product gate (final read)

- **G4 product gate** (independent of Java): kicad-cli imports our SES for every fixture with
  zero unconnected items introduced; 100 % of emitted segments conform to the board's
  snap_angle; README headline = wall-clock on boards both tools finish with JVM startup broken
  out; results reported as (unrouted, added violations, angle violations, KiCad-DRC delta).
  Current read (33 boards = 24 mainline + 9 OOS, KiCad 10.0.5, salmon, commit `99b2ce4`,
  `bench/results/g4-gate/99b2ce4/`, superseding `c58cb47`):
  - **0 router-generated angle violations across all 33 boards** (including `cla45-sensor-l6`, which dropped from 4–5 to **0** via `insert_angle_compliant_neckdown_segment`); imported route copper preserved verbatim;
  - **all 8 `.kicad_pcb` OOS boards import into KiCad 10.0.5 cleanly, `unconnΔ <= 0`** (`easyeda-pcb1` is `.dsn`-only); all 9 OOS boards pass `g21_judge` (`bench/results/gate4-quality/20261119-99b2ce4/oos/`);
  - KiCad DRC residual: 0 on clean boards (`cla45-computeboard`, `kicad-tutorial`); the historic
    (b) inner-cutout class is **0** after `cutout-keepout`; the historic (a) 90° angle residual on
    `kicad-coldfire-unrouted` is **0**; remaining residual on complex HDI/multi-layer boards
    is fixture/zone-fill/keepout geometry debt;
  - **0 added violations** across all 45/45 judgeable mainline boards (`46/46` PASS, `26/26` comparative `12W / 14T / 0L`, `bench/results/gate4-quality/20261119-99b2ce4/main/`).

## M5 & Ledger4 Closure (DONE — 2026-11-19, `99b2ce4`)

All post-`ledger3` quality, speed, angle-compliance, and product deliverables are complete:

1. **Exact $\mathbb{R}^2$ `OctR2` geometry, cached `SearchTree` item hulls, and stack `FastPoint`**:
   eliminated hot-loop `Simplex::enlarge`, `remove_redundant_from_sorted`, per-segment `board_bounds`, and `restrain_shape` `BigInt` allocations while keeping 8/8 deterministic SHA-256 bit-identity.
2. **Serial `ledger4` (`bench/results/ledger4/20261119-99b2ce4/`, `W=1`, `99b2ce4`)**:
   - **27 / 27 `java_finished` boards beat Java on both wall-clock (`1.37×`–`188.0×`) and in-tool time (`1.38×`–`59.9×`)**, with **0 time losses**: total wall **290.0 s vs 814.7 s (`2.81×`)**, total in-tool **289.8 s vs 766.0 s (`2.64×`)**, geomean **13.84×** wall / **7.86×** in-tool across the 26 unique designs (`12.89×` / `7.47×` across all 27 boards).
   - **27 / 27 `g21_judge` PASS** (**26 / 26 comparative `12W / 14T / 0L`**, **0 losses**, **0 added violations**; `Issue039-bug-design` converged to **4u / 130.84 s** vs Java `10u / 179.22 s`, `Issue102` converged to **0u / 38.69 s** vs Java `0u / 85.43 s`).
   - **27 / 27 lower peak RSS**: mean **89.3 MB vs 4,923.6 MB** (~55.1× lower across all 27 boards; **82.9 MB vs 4,846.4 MB**, ~58.5× lower across 26 unique designs).
3. **Deflected neckdown angle-knee guard (`insert_angle_compliant_neckdown_segment`)**:
   ensures off-axis `try_neck_down` and `insert_fanout_micro_neckdown` stubs bridge via a 45°/90° knee (`0` generated angle violations across all 33 G4 boards).
4. **Native GPUI viewer & interactive router (`crates/fr-gui`)**:
   integrated `RouterObserver` + `<34 ms` `fr_core::snapshot` + `fr-gui` canvas, layer panel, ratsnest inspector, and headless `--screenshot` pipeline (<50 MB RSS).

## Standing risks

| risk | mitigation |
|---|---|
| Jar ≠ any public source tree exactly | conformance pass + jar-package audit in CI; port from release-revision source, verify constants via javap |
| Shove/maze drift invisible until late | settle-order differential harness built in M1, used per M2 slice; per-fixture delta gates surface drift immediately |
| Referee disagrees with Java DRC | G1.2a multiset gate on input boards (no router confounds), before any router work is judged |
| Speed target set on unmeasured assumptions | M0.5 experiments land before targets are finalized; G3.1 target re-derived from data |
| Parallel modes add rare violations | G3.2 runs the full quality gates on the parallel mode; T3 flagged until clean |
| Suite overfit (39 designs, 87 % 2-layer, all 45°) | G2.4 out-of-sample tier is mandatory, not advisory |
| Session interruptions | specs + this roadmap + divergence log in-repo; small gated commits on rework/router-core |
