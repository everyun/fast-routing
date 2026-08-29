# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-08-30T00:24:20
- Config label: `wide-base`
- Passes: 1, timeout: 150.0s
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/tmp/fr-baseline/target/release/fr-cli`
- Repo rev: `1fc943f`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-08-29T00:01:45Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean.

| fixture | class | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue022-AutoRouter_interrupted.dsn | timeout | - | - | - | - | 55/531 | 140.11 (140.11) | 45M | 203/204 | - | - | java_timeout |
| Issue034-Green14SegLED.dsn | finished | - | - | - | - | 0/0 | 72.58 (72.57) | 35M | 101/101 | - | - | java did not finish |
| Issue054-tairakb.dsn | finished | - | - | - | - | 1/5 | 68.81 (68.79) | 28M | 55/56 | - | - | java did not finish |
| Issue066-Project_GP8B.dsn | timeout | - | - | - | - | 607/23 | 140.06 (140.05) | 186M | 63/229 | - | - | java_timeout |
| Issue102-Mars-64-revE-rot00.dsn | finished | - | - | - | - | 9/64 | 140.38 (140.37) | 59M | 74/74 | - | - | java did not finish |
| Issue107-freq_teiler_200kHz_kicad.dsn | finished | - | - | - | - | 0/0 | 7.64 (7.63) | 20M | 59/59 | - | - | java did not finish |
| Issue113-Protein.dsn | timeout | - | - | - | - | 133/44 | 140.09 (140.08) | 98M | 139/160 | - | - | java_timeout |
| Issue153-wavefolder.dsn | finished | - | - | - | - | 1/9 | 11.49 (11.49) | 16M | 29/30 | - | - | java did not finish |
| Issue157-TeamAdapt-LinePCB.dsn | finished | - | - | - | - | 14/18 | 140.49 (140.48) | 125M | 64/64 | - | - | java did not finish |
| Issue163-pic_programmer.dsn | finished | - | - | - | - | 7/6 | 4.86 (4.85) | 11M | 33/34 | - | - | java did not finish |

## Summary

- **rust**: 10/10 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 827, violations 700; total wall 866.5s; checker completion 820/1011 nets (81.1%)
- **Fixture classes** (Java reference): java_finished 7, java_timeout 3
- **Quality filter** `placeholder-nets-v0`: 0/10 fixtures pass
- **Geomean speedup, quality-filtered**: n/a — 0 fixtures passed `placeholder-nets-v0`. No speed claim may be made from this run.

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction.
