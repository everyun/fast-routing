# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-08-30T00:05:34
- Config label: `baseline-mp1`
- Passes: 1, timeout: 300.0s
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/tmp/fr-baseline/target/release/fr-cli`
- Repo rev: `1fc943f`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-08-29T00:01:45Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean.

| fixture | class | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue006-LPC18XX_43XX_SCH.dsn | degenerate | - | - | - | - | CRASH | 0.43 | 2M | - | - | - | java_degenerate |
| Issue026-J2_reference.dsn | finished | - | - | - | - | 0/0 | 28.38 (28.38) | 19M | 12/12 | - | - | java did not finish |
| Issue034-Green14SegLED.dsn | finished | - | - | - | - | 0/0 | 70.60 (70.59) | 37M | 101/101 | - | - | java did not finish |
| Issue093-interf_u.dsn | finished | - | - | - | - | 19/192 | 12.42 (12.41) | 43M | 110/110 | - | - | java did not finish |
| Issue110-testPCBSpecctraFile.dsn | finished | - | - | - | - | 21/41 | 30.02 (30.01) | 49M | 24/25 | - | - | java did not finish |
| Issue110-testProjectFromFreeroutingBugTest.dsn | finished | - | - | - | - | 0/0 | 0.06 (0.05) | 6M | 4/4 | - | - | java did not finish |
| Issue110-testProjectFromFreeroutingBugTest01.dsn | finished | - | - | - | - | 0/0 | 0.06 (0.05) | 6M | 4/4 | - | - | java did not finish |
| Issue143-rpi_splitter.dsn | finished | - | - | - | - | 0/0 | 0.47 (0.46) | 8M | 4/4 | - | - | java did not finish |
| Issue143-rpi_splitter_mod.dsn | finished | - | - | - | - | 0/0 | 0.01 (0.01) | 5M | 1/4 | - | - | java did not finish |
| Issue155-CH376_MCP795_Module.dsn | finished | - | - | - | - | 0/0 | 10.31 (10.30) | 17M | 25/25 | - | - | java did not finish |
| Issue159-setonix_2hp-pcb.dsn | finished | - | - | - | - | 0/0 | 0.61 (0.60) | 9M | 6/6 | - | - | java did not finish |
| Issue187-processor.Z80.dsn | finished | - | - | - | - | 0/0 | 1.16 (1.15) | 34M | 245/245 | - | - | java did not finish |
| Issue229-display-8-digit-hc595.dsn | degenerate | - | - | - | - | 20/0 | 1.16 (1.15) | 8M | 30/30 | - | - | java_degenerate |
| Issue230-CNH_Functional_Tester_1.dsn | finished | - | - | - | - | 5/16 | 222.90 (222.86) | 82M | 59/64 | - | - | java did not finish |

## Summary

- **rust**: 13/14 runs finished (0 timeouts, 1 crashes); self-reported total unrouted 65, violations 249; total wall 378.2s; checker completion 625/634 nets (98.6%)
- **Fixture classes** (Java reference): java_degenerate 2, java_finished 12
- **Quality filter** `placeholder-nets-v0`: 0/14 fixtures pass
- **Geomean speedup, quality-filtered**: n/a — 0 fixtures passed `placeholder-nets-v0`. No speed claim may be made from this run.

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction.
