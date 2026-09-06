# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-09-06T12:07:33
- Config label: `ledger2-mp20-300s`
- Passes: 20, timeout: 300.0s
- Concurrency: W=1 (`--serial-perf`: timings are quotable)
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/Users/chiqingzhen/fr-work/ledger2/target/release/fr-cli`
- Repo rev: `4f537aa`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-09-04T21:56:39Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean; `W` = how many bench processes were running at once when this row was timed (`?` = a run recorded before the field existed). **Every timing on a `W > 1` row is inflated by contention and is excluded from the geomean whatever `quality` says** — see `--serial-perf`.

| fixture | class | W | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue026-J2_reference.dsn | finished | 1 | 0/0 | 4.00 (1.66) | 2815M | 12/12 | 0/0 | 0.42 (0.40) | 11M | 12/12 | 9.5x | 4.1x | pass |
| Issue034-Green14SegLED.dsn | finished | 1 | 9/0 | 30.00 (29.42) | 6162M | 97/101 | 0/0 | 11.91 (11.90) | 26M | 101/101 | 2.5x | 2.5x | pass |
| Issue035-ReadPlaceScope.dsn | finished | 1 | 4/152 | 44.59 (43.39) | 5470M | 93/93 | 0/152 | 10.78 (10.77) | 97M | 93/93 | 4.1x | 4.0x | pass |
| Issue039-bug-design.dsn | finished | 1 | 10/25 | 202.25 (203.99) | 6210M | 74/74 | 30/25 | 284.41 (284.39) | 104M | 72/74 | 0.7x | 0.7x | checker nets 72 < java 74 |
| Issue054-tairakb.dsn | finished | 1 | 5/5 | 119.69 (118.50) | 4641M | 55/56 | 1/5 | 78.37 (78.35) | 153M | 55/56 | 1.5x | 1.5x | pass |
| Issue093-interf_u.dsn | finished | 1 | 19/192 | 18.42 (15.74) | 5656M | 110/110 | 0/192 | 0.86 (0.85) | 15M | 110/110 | 21.4x | 18.5x | pass |
| Issue102-Mars-64-revE-rot00.dsn | finished | 1 | 0/64 | 95.96 (95.81) | 6075M | 74/74 | 1/64 | 134.38 (134.37) | 76M | 74/74 | 0.7x | 0.7x | pass |
| Issue103-Board-Routed.dsn | finished | 1 | 0/63 | 5.35 (2.03) | 5666M | 267/267 | 0/63 | 0.78 (0.77) | 24M | 267/267 | 6.9x | 2.6x | pass |
| Issue107-freq_teiler_200kHz_kicad.dsn | finished | 1 | 0/0 | 6.82 (3.96) | 6162M | 59/59 | 0/0 | 1.05 (1.04) | 18M | 59/59 | 6.5x | 3.8x | pass |
| Issue107-freq_teiler_200kHz_kicad_bad.dsn | finished | 1 | 0/0 | 6.82 (4.10) | 6161M | 59/59 | 0/0 | 1.20 (1.19) | 17M | 59/59 | 5.7x | 3.4x | pass |
| Issue110-Pajalnaja_stancija.dsn | finished | 1 | 6/18 | 72.31 (69.53) | 6922M | 39/41 | 6/18 | 102.83 (102.82) | 295M | 39/41 | 0.7x | 0.7x | pass |
| Issue110-RelayModule.dsn | finished | 1 | 0/3 | 9.84 (7.76) | 6157M | 44/44 | 0/4 | 16.74 (16.73) | 52M | 44/44 | 0.6x | 0.5x | pass |
| Issue110-testPCBSpecctraFile.dsn | finished | 1 | 21/41 | 28.44 (32.77) | 6126M | 24/25 | 3/41 | 9.02 (9.01) | 49M | 24/25 | 3.2x | 3.6x | pass |
| Issue110-testProjectFromFreeroutingBugTest.dsn | finished | 1 | 0/0 | 3.23 (0.45) | 733M | 4/4 | 0/0 | 0.03 (0.02) | 6M | 4/4 | 97.8x | 22.5x | pass |
| Issue110-testProjectFromFreeroutingBugTest01.dsn | finished | 1 | 0/0 | 3.26 (0.43) | 720M | 4/4 | 0/0 | 0.03 (0.02) | 6M | 4/4 | 98.7x | 21.5x | pass |
| Issue110-Паяльная станция.dsn | finished | 1 | 6/18 | 68.25 (65.50) | 6943M | 39/41 | 6/18 | 103.46 (103.44) | 293M | 39/41 | 0.7x | 0.6x | byte-identical duplicate of Issue110-Pajalnaja_stancija.dsn |
| Issue143-rpi_splitter.dsn | finished | 1 | 0/0 | 3.26 (0.64) | 888M | 4/4 | 0/0 | 0.06 (0.05) | 6M | 4/4 | 57.1x | 12.8x | pass |
| Issue143-rpi_splitter_mod.dsn | finished | 1 | 0/0 | 2.75 (0.28) | 398M | 1/4 | 0/0 | 0.01 (0.00) | 5M | 1/4 | 196.4x | - | pass |
| Issue153-wavefolder.dsn | finished | 1 | 3/9 | 6.32 (3.51) | 6170M | 28/30 | 0/9 | 1.07 (1.06) | 14M | 29/30 | 5.9x | 3.3x | pass |
| Issue155-CH376_MCP795_Module.dsn | finished | 1 | 0/0 | 5.31 (3.04) | 6917M | 25/25 | 0/0 | 2.68 (2.67) | 18M | 25/25 | 2.0x | 1.1x | pass |
| Issue157-TeamAdapt-LinePCB.dsn | finished | 1 | 14/18 | 84.90 (88.47) | 4981M | 64/64 | 2/18 | 91.45 (91.44) | 194M | 64/64 | 0.9x | 1.0x | pass |
| Issue159-setonix_2hp-pcb.dsn | finished | 1 | 0/0 | 3.26 (0.77) | 1407M | 6/6 | 0/0 | 0.25 (0.24) | 10M | 6/6 | 13.2x | 3.2x | pass |
| Issue163-pic_programmer.dsn | finished | 1 | 9/6 | 7.83 (5.26) | 6064M | 33/34 | 0/6 | 3.18 (3.17) | 41M | 33/34 | 2.5x | 1.7x | pass |
| Issue187-processor.Z80.dsn | finished | 1 | 0/0 | 4.81 (0.92) | 5672M | 0/245 | 0/0 | 1.05 (1.04) | 31M | 245/245 | 4.6x | 0.9x | pass |
| Issue209-split10.dsn | finished | 1 | 2/182 | 24.96 (22.71) | 6169M | 40/45 | 0/182 | 3.86 (3.85) | 30M | 40/45 | 6.5x | 5.9x | pass |
| Issue217-8088sbc.dsn | finished | 1 | 7/0 | 20.92 (18.48) | 6046M | 71/76 | 0/0 | 11.19 (11.18) | 48M | 76/76 | 1.9x | 1.7x | pass |
| Issue230-CNH_Functional_Tester_1.dsn | finished | 1 | 2/16 | 32.49 (29.67) | 4394M | 61/64 | 0/16 | 18.50 (18.49) | 226M | 62/64 | 1.8x | 1.6x | pass |

## Summary

- **java**: 27/27 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 117, violations 812; total wall 916.0s; checker completion 1387/1661 nets (83.5%)
- **rust**: 27/27 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 49, violations 813; total wall 889.6s; checker completion 1641/1661 nets (98.8%)
- **Fixture classes** (Java reference): java_finished 27
- **Quality filter** `placeholder-nets-v0`: 25/27 fixtures pass; 1 byte-identical duplicate(s) excluded from the filtered aggregate
- **Geomean speedup (java/rust), quality-filtered, serial (W=1)**: wall-clock 5.5x (n=25), in-tool 2.8x (n=24)
- Quality-filtered set: Issue026-J2_reference.dsn, Issue034-Green14SegLED.dsn, Issue035-ReadPlaceScope.dsn, Issue054-tairakb.dsn, Issue093-interf_u.dsn, Issue102-Mars-64-revE-rot00.dsn, Issue103-Board-Routed.dsn, Issue107-freq_teiler_200kHz_kicad.dsn, Issue107-freq_teiler_200kHz_kicad_bad.dsn, Issue110-Pajalnaja_stancija.dsn, Issue110-RelayModule.dsn, Issue110-testPCBSpecctraFile.dsn, Issue110-testProjectFromFreeroutingBugTest.dsn, Issue110-testProjectFromFreeroutingBugTest01.dsn, Issue143-rpi_splitter.dsn, Issue143-rpi_splitter_mod.dsn, Issue153-wavefolder.dsn, Issue155-CH376_MCP795_Module.dsn, Issue157-TeamAdapt-LinePCB.dsn, Issue159-setonix_2hp-pcb.dsn, Issue163-pic_programmer.dsn, Issue187-processor.Z80.dsn, Issue209-split10.dsn, Issue217-8088sbc.dsn, Issue230-CNH_Functional_Tester_1.dsn
- **quality-unfiltered, not comparable** (every serially timed fixture both tools finished, no quality check whatsoever — includes boards where we route a fraction of the nets): wall-clock 4.7x (n=27), in-tool 2.5x (n=26)

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction. Every ratio above is from `W=1` rows only; a sharded sweep produces quality verdicts and no speed number at all.
