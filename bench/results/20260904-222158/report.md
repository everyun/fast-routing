# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-09-04T22:21:58
- Config label: `smoke-user-data`
- Passes: 20, timeout: 90.0s
- Concurrency: W=1
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/Users/chiqingzhen/fr-work/infra/target/release/fr-cli`
- Repo rev: `9876171`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-08-29T00:01:45Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean; `W` = how many bench processes were running at once when this row was timed (`?` = a run recorded before the field existed). **Every timing on a `W > 1` row is inflated by contention and is excluded from the geomean whatever `quality` says** — see `--serial-perf`.

| fixture | class | W | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue026-J2_reference.dsn | finished | 1 | 0/0 | 3.84 (1.69) | 2860M | 12/12 | - | - | - | - | - | - | rust did not finish |

## Summary

- **java**: 1/1 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 0, violations 0; total wall 3.8s; checker completion 12/12 nets (100.0%)
- **Fixture classes** (Java reference): java_finished 1
- **Quality filter** `placeholder-nets-v0`: 0/1 fixtures pass
- **Geomean speedup, quality-filtered**: n/a — 0 fixtures passed `placeholder-nets-v0`. No speed claim may be made from this run.

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction. Every ratio above is from `W=1` rows only; a sharded sweep produces quality verdicts and no speed number at all.
