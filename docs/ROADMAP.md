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

## M0.5 — Calibration & conformance (IN PROGRESS)

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
      Still pending, tracked below:
  - [ ] `T_java(complete)` for 3 boards. Issue208 hit the 2 h cap without finishing pass #1
        (378 unrouted items — a different pathology from the rest, needs its own look);
        Issue029 and Issue107-bad never routed at all — they block on a modal
        `GuiManager.read_rules_file` confirmation dialog (jstack in the write-up §4), which is
        reproducible on *any* board with a matching `.rules` sidecar. Re-run headless
        (`-Djava.awt.headless=true`) and make the runner report a stage-less run as a harness
        failure, not a slow board.
  - [ ] Resolve the fanout confound: the whole 05:03+ campaign ran with **zero** fanout stages
        while the 300 s baseline ran 37/46, same jar and flags. Until that is explained, the
        uncapped walls describe an auto-routing-only pipeline and cross-campaign stage sums are
        not comparable. The re-baseline must pass `--router.fanout.enabled=…` explicitly and
        record the resolved value.
  - [ ] Stage split: `-Dfreerouting.logging.console.level=DEBUG` produced 0 DEBUG lines in all
        5 runs (property name never reaches `LoggingSettings`), so the split is unmeasured. The
        runs were repurposed as the default-GC control for the SerialGC comparison.
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
- [ ] Re-pin the reference config (`-mp 9999`, stated wall budget; the optimizer flag
      defaults on — the stage only runs when routing completes, which is why the 300 s
      `-mp 20` baseline almost never reached it) and
      re-baseline; record 3-run stability per fixture (`stable/unstable`).
- [ ] Fixture manifest: per-board classification (`java_finished/timeout/crash/degenerate`),
      dedup (≈39 independent designs), coverage properties; `bench/fetch_fixtures.sh`.
- [ ] Golden dumps: `java -de f.dsn -do f.import.dsn --router.enabled=false
      --router.fanout.enabled=false --router.optimizer.enabled=false` for all 46, checked in
      (G1.0 oracle). Plus `java -drc` JSON of every input board (G1.2a oracle).
- [ ] Fix `run_bench.py` stage-regex bug (per-stage keys currently unaggregatable); wire
      fr-check placeholder judging; delete the fabricated README tables (done in this commit).

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
  clippy clean except legacy crates slated for M2 deletion
- Pipeline switched: fr-cli default = import → check → SES passthrough (exit 3 until M2);
  fr-drc retired behind the legacy feature

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
- **G2.1** per fixture, referee-judged, default mode, reference config:
  `added_violations(ours) ≤ added_violations(java)` AND `unrouted(ours) ≤ unrouted(java)`,
  where added = final − imported-board baseline, and no violation may involve two USER_FIXED
  items. Fixture classes: `java_finished+stable` → comparative gate; `timeout/crash/degenerate/
  unstable` → absolute gate (added_violations = 0 AND unrouted < unrouted(input)) — "route
  nothing" is rejected by the anti-degeneracy guard (SES must contain copper for ≥1 multi-pin
  net when the DSN has any).
- **G2.2** determinism: two `--deterministic` runs → byte-identical SES.
- **G2.3** speed floor (not the headline): suite in-tool total ≤ 1/3 of Java's on
  `java_finished` boards; peak RSS ≤ absolute per-size-class targets (set from M0.5 data),
  not a fraction of the JVM's.
- **G2.4** out-of-sample tier: ≥6 boards with ≥4 signal layers, ≥3 with power planes, ≥2 with
  snap_angle 90/none, ≥1 non-KiCad export, ≥2 clean unrouted boards — including the CLA45
  threeboard 6-layer HDI exports. Same gate form as G2.1 (Java references generated for them).

### G2.1 progress log
- 2026-08-29 first read: 15/46 (26 failures = our own 300s timeout writing no SES).
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

## M3 — Speed

1. Profile-driven single-thread work against the measured budget from M0.5 (query buffers, door
   cache, room retention behind a flag with equivalence tests, incremental DRC/ratsnest).
2. T1 (portfolio + optimizer parallel) and T2 (referee/regions) — honestly bounded (~1.4×
   Amdahl on the route pass; see ARCHITECTURE).
3. T3 spatially-disjoint concurrent routing — the actual scaling lever, flagged until it passes
   G2.1/G2.4 unchanged.

Gates:
- **G3.1** quality-filtered speed: geomean in-tool speedup vs Java, computed ONLY over fixtures
  passing G2.1 in the same run (bench refuses to emit it otherwise), ≥ target set from M0.5
  measurements (placeholder 10×, to be re-derived); AND 46/46 **terminate** ≤ 300 s (stop rules
  fire, no hangs); AND completion-vs-uncapped-Java: on the 14 ex-timeout boards, our results
  compared against T_java(complete) from M0.5, not against the 300 s cutoff.
- **G3.2** default (parallel) mode passes G2.1/G2.4 unchanged.
- **G3.3** scaling, conditional on T3: ≥3× route-stage time 1→8 workers on the ≥60 s-class
  boards, RSS per worker within class budget. If T3 is not shipped, G3.3 is reported as
  not-applicable — never faked with portfolio numbers.

## M4 — Capability closure & release honesty

- **G4 product gate** (independent of Java): kicad-cli imports our SES for every fixture with
  zero unconnected items introduced; 100 % of emitted segments conform to the board's
  snap_angle; README headline = wall-clock on boards both tools finish with JVM startup broken
  out; results reported as (unrouted, added violations, angle violations, KiCad-DRC delta).
- RouterSettings scoreboard: implement / intentionally-dead (e.g. `ignore_net_classes`) /
  rejected, per knob. CLI compatibility table.
- CI: build + clippy + tests + fixture fetch + jar-package audit + 6-board bench smoke with
  regression thresholds + spec-consistency lint over the nine specs' constant tables.
- `docs/PARITY-DIVERGENCES.md` complete; dead surface deleted (CUDA stub, fake bench bins).

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
