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

- **Ripped connections are requeued inside the pass** (`fr_router::batch::BatchRouter`,
  `requeue_ripped_items`, default on).
  *Upstream* (`autoroute/BatchAutorouter.java:431-694`) derives the work list once per pass
  (`getAutorouteItems`, `:250-306`) and never adds to it; a net whose wiring a later connection
  ripped is picked up by the *next* pass, which re-derives the list from scratch and retries at
  `start_ripup_costs * pass_no` (§4.1: "there is **no retry inside a pass**").
  *We* re-enqueue a net whose copper an insert deleted, once per ripup, capped at the pass's
  own work-list length so a pass can at most double.
  *Why*: with `-mp 1` — the M2a default and what the CLI ships — there is no next pass, so a
  ripup would otherwise be a pure loss: the router would trade a routed connection for an
  unrouted one and report success.
  *Effect*: on `-mp 1` this can only reduce incompletes. On `-mp >= 2` it front-loads work the
  release would have done in the following pass, at that pass's *lower* ripup budget, so the
  intermediate boards differ from upstream's even when the final one does not. Revisit when the
  M2c pass scheduler lands: the honest long-run configuration is `requeue_ripped_items = false`
  plus upstream's stop rules.

## Referee

(none yet)

## Rejected divergences (things we deliberately do NOT fix)

- `class_class` inner-iterator pairing bug: replicated (identical clearance matrix). No fixture
  exercises it; real-world files may — revisit only with a test case.
- `ignore_net_classes` no-op headless: replicated as intentionally dead.
- `ClearanceMatrix.getValue` out-of-range → 0: replicated.
- `splitInsideDrillPadProhibited` `&&`/`||` precedence bug: replicated (routing-visible).
