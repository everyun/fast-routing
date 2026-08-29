# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-08-30T00:39:25
- Config label: `wide-mine`
- Passes: 1, timeout: 150.0s
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/tmp/fr-mine/target/release/fr-cli`
- Repo rev: `1fc943f`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-08-29T00:01:45Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean.

| fixture | class | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue006-LPC18XX_43XX_SCH.dsn | degenerate | - | - | - | - | CRASH | 0.01 | 2M | - | - | - | java_degenerate |
| Issue022-AutoRouter_interrupted.dsn | timeout | - | - | - | - | 55/531 | 140.12 (140.11) | 47M | 203/204 | - | - | java_timeout |
| Issue026-J2_reference.dsn | finished | - | - | - | - | 0/0 | 2.59 (2.59) | 13M | 12/12 | - | - | java did not finish |
| Issue034-Green14SegLED.dsn | finished | - | - | - | - | 0/0 | 65.04 (65.03) | 34M | 101/101 | - | - | java did not finish |
| Issue054-tairakb.dsn | finished | - | - | - | - | 1/5 | 58.18 (58.17) | 27M | 55/56 | - | - | java did not finish |
| Issue066-Project_GP8B.dsn | timeout | - | - | - | - | 637/23 | 140.04 (140.03) | 245M | 56/229 | - | - | java_timeout |
| Issue093-interf_u.dsn | finished | - | - | - | - | 19/192 | 12.26 (12.25) | 42M | 110/110 | - | - | java did not finish |
| Issue102-Mars-64-revE-rot00.dsn | finished | - | - | - | - | 10/64 | 140.50 (140.50) | 59M | 74/74 | - | - | java did not finish |
| Issue107-freq_teiler_200kHz_kicad.dsn | finished | - | - | - | - | 0/0 | 7.71 (7.71) | 19M | 59/59 | - | - | java did not finish |
| Issue110-testPCBSpecctraFile.dsn | finished | - | - | - | - | 21/41 | 27.50 (27.49) | 49M | 24/25 | - | - | java did not finish |
| Issue110-testProjectFromFreeroutingBugTest.dsn | finished | - | - | - | - | 0/0 | 0.06 (0.05) | 6M | 4/4 | - | - | java did not finish |
| Issue110-testProjectFromFreeroutingBugTest01.dsn | finished | - | - | - | - | 0/0 | 0.06 (0.05) | 6M | 4/4 | - | - | java did not finish |
| Issue113-Protein.dsn | timeout | - | - | - | - | 133/44 | 140.06 (140.06) | 101M | 139/160 | - | - | java_timeout |
| Issue143-rpi_splitter.dsn | finished | - | - | - | - | 0/0 | 0.46 (0.46) | 8M | 4/4 | - | - | java did not finish |
| Issue143-rpi_splitter_mod.dsn | finished | - | - | - | - | 0/0 | 0.02 (0.01) | 5M | 1/4 | - | - | java did not finish |
| Issue153-wavefolder.dsn | finished | - | - | - | - | 1/9 | 11.40 (11.39) | 15M | 29/30 | - | - | java did not finish |
| Issue155-CH376_MCP795_Module.dsn | finished | - | - | - | - | 0/0 | 10.27 (10.26) | 19M | 25/25 | - | - | java did not finish |
| Issue157-TeamAdapt-LinePCB.dsn | finished | - | - | - | - | 14/18 | 140.49 (140.48) | 124M | 64/64 | - | - | java did not finish |
| Issue159-setonix_2hp-pcb.dsn | finished | - | - | - | - | 0/0 | 0.60 (0.60) | 10M | 6/6 | - | - | java did not finish |
| Issue163-pic_programmer.dsn | finished | - | - | - | - | 7/6 | 4.89 (4.88) | 10M | 33/34 | - | - | java did not finish |
| Issue187-processor.Z80.dsn | finished | - | - | - | - | 0/0 | 1.09 (1.08) | 32M | 245/245 | - | - | java did not finish |
| Issue229-display-8-digit-hc595.dsn | degenerate | - | - | - | - | 20/0 | 0.31 (0.30) | 8M | 30/30 | - | - | java_degenerate |
| Issue230-CNH_Functional_Tester_1.dsn | finished | - | - | - | - | 5/16 | 140.28 (140.27) | 95M | 59/64 | - | - | java did not finish |

## Summary

- **rust**: 22/23 runs finished (0 timeouts, 1 crashes); self-reported total unrouted 923, violations 949; total wall 1043.9s; checker completion 1337/1544 nets (86.6%)
- **Fixture classes** (Java reference): java_degenerate 2, java_finished 18, java_timeout 3
- **Quality filter** `placeholder-nets-v0`: 0/23 fixtures pass
- **Geomean speedup, quality-filtered**: n/a — 0 fixtures passed `placeholder-nets-v0`. No speed claim may be made from this run.

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction.
