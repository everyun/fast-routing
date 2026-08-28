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

- **Fanout no-progress stop** (`fr_router::fanout`, `NO_PROGRESS_PASS_LIMIT = 2`).
  *Upstream* (`autoroute/BatchFanout.java:82-144`) stops a fanout run on three conditions only:
  `routed_count == 0` (`:111-113`), three consecutive **bit-identical** `(routed_count << 32) ^
  via_count` pass signatures (`:114-130`, `STAGNATION_PASS_LIMIT = 3`), and an unchanged board
  hash (`:134-138`). The source comment at `:115-119` records the failure that motivated the
  oscillation detector: **14 identical passes** on a dense SMD carrier (Issue219-LogicBoard_smt)
  where pins kept ripping each other's escapes while the ripup budget escalated uselessly — the
  detector only caught it because the signatures happened to repeat exactly.
  *We* additionally stop after 2 consecutive passes whose via count did not grow, i.e. passes
  that escaped no *new* pin. Upstream's three stops are all kept and evaluated first.
  *Why*: the release's rule needs the per-pass outcome to repeat **exactly**; a cycle that
  alternates between two states with different via counts runs to `fanout.maxPasses` (20). Ours
  keys on progress rather than on repetition.
  *Effect*: on an oscillating board the stage stops strictly earlier and never later, so the
  fanout via count is a subset of upstream's and the route stage starts from a board with
  fewer, not more, escapes. On a converging board the two rules agree — every pass that escapes
  a new pin grows the via count, so the counter never advances. Bounded and one-sided; measure
  on Issue219 when the fanout gate runs.

- **Ripped connections are requeued inside the pass, at `-mp 1` only**
  (`fr_router::batch::schedule::RequeuePolicy`, default `SinglePassOnly`).
  *Upstream* (`autoroute/BatchAutorouter.java:431-694`) derives the work list once per pass
  (`getAutorouteItems`, `:250-306`) and never adds to it; a net whose wiring a later connection
  ripped is picked up by the *next* pass, which re-derives the list from scratch and retries at
  `start_ripup_costs * pass_no` (§4.1: "there is **no retry inside a pass**").
  *We* re-enqueue a net whose copper an insert deleted, once per ripup, capped at the pass's
  own work-list length so a pass can at most double — **but only when the run was asked for a
  single pass**. At `-mp >= 2` the policy resolves to upstream's rule and nothing is requeued.
  *Why*: with `-mp 1` there is no next pass, so a ripup would otherwise be a pure loss: the
  router would trade a routed connection for an unrouted one and report success. Now that the
  M2c pass scheduler has landed, `-mp >= 2` *has* the next pass upstream relies on, so the
  divergence has no reason to exist there and is switched off — as the earlier version of this
  entry said it should be.
  *Effect*: on `-mp 1` this can only reduce incompletes (measured on Issue026: one connection
  requeued in-pass). On `-mp >= 2` there is now **no divergence**: pass boundaries, work lists
  and ripup budgets are the release's. `RequeuePolicy::Always` keeps the old behaviour available
  for A/B measurement of the divergence itself; `tests/batch_multipass.rs` asserts the switch.
  The optimizer's inner router (`optimizer::MazeRerouter`, upstream's
  `autoroute_passes_for_optimizing_item`) drives `autoroute_pass` directly rather than through
  `run`, so it resolves the flag itself with `BatchRouter::set_requeue_active(false)`: that loop
  *has* the next pass upstream relies on, so the divergence has no reason to exist there either.

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

- **Fork/end smoothing (`smoothen_start/end_corner_at_trace`) is not ported for either
  pull-tight flavor** (`fr_router::pull_tight`, `fr_router::pull_tight_any`).
  *Upstream* has it in `PullTightAlgo45` (`:415`, `:520`) and in `PullTightAlgoAnyAngle`
  (`:717`, `:835`); it is `opt_changed_area`'s `else if` branch (`PullTightAlgo.java:128-131`).
  *We* return `false` from that branch in both flavors. It needs the
  `insert_trace_without_cleaning` + `split_traces` + `normalize_traces` triple per improvement
  round, which is the one part of the family that has not been wired.
  *Why symmetric*: porting it for one angle mode only would make the two flavors disagree for a
  reason that has nothing to do with the angle mode, which is exactly the kind of drift the
  differential harness cannot attribute.
  *Effect*: traces that fork or end on another trace keep an acute angle upstream would chamfer.
  One-sided — it can only leave a trace longer, never introduce a violation.

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
  budget, polled between connections and between passes, which ends the run through the same
  tail as every stop rule — best retained board restored, referee run, session written — and
  clamps each connection's `TimeLimit` to what the job has left so one connection cannot overrun
  it by its own 100 s. Off by default: a run without `--job-timeout` reads no clock and is
  unchanged. *Why*: a harness cap that `SIGKILL`s the process produces no session at all, which
  is strictly less information than the board we were holding. 26 of the 46 boards in the first
  full-suite G2.1 read failed exactly that way.

## Optimizer

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

## Referee

(none yet)

## Rejected divergences (things we deliberately do NOT fix)

- `class_class` inner-iterator pairing bug: replicated (identical clearance matrix). No fixture
  exercises it; real-world files may — revisit only with a test case.
- `ignore_net_classes` no-op headless: replicated as intentionally dead.
- `ClearanceMatrix.getValue` out-of-range → 0: replicated.
- `splitInsideDrillPadProhibited` `&&`/`||` precedence bug: replicated (routing-visible).
