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

(none yet — router not started; budget policy intentionally NOT divergent: upstream wall-clock
constants are the default, see ARCHITECTURE "Budgets")

## Referee

(none yet)

## Rejected divergences (things we deliberately do NOT fix)

- `class_class` inner-iterator pairing bug: replicated (identical clearance matrix). No fixture
  exercises it; real-world files may — revisit only with a test case.
- `ignore_net_classes` no-op headless: replicated as intentionally dead.
- `ClearanceMatrix.getValue` out-of-range → 0: replicated.
- `splitInsideDrillPadProhibited` `&&`/`||` precedence bug: replicated (routing-visible).
