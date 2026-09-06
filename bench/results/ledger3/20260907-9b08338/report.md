# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-09-07T00:37:11
- Config label: `ledger3-mp20-300s`
- Passes: 20, timeout: 300.0s
- Concurrency: W=1 (`--serial-perf`: timings are quotable)
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/Users/chiqingzhen/fr-work/ledger2/target/release/fr-cli`
- Repo rev: `9b08338`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-09-04T21:56:39Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean; `W` = how many bench processes were running at once when this row was timed (`?` = a run recorded before the field existed). **Every timing on a `W > 1` row is inflated by contention and is excluded from the geomean whatever `quality` says** — see `--serial-perf`.

| fixture | class | W | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue026-J2_reference.dsn | finished | 1 | 0/0 | 3.87 (1.63) | 2855M | 12/12 | 0/0 | 0.40 (0.39) | 10M | 12/12 | 9.7x | 4.2x | pass |
| Issue034-Green14SegLED.dsn | finished | 1 | 9/0 | 29.51 (29.07) | 6215M | 97/101 | 0/0 | 11.60 (11.59) | 25M | 101/101 | 2.5x | 2.5x | pass |
| Issue035-ReadPlaceScope.dsn | finished | 1 | 4/152 | 45.57 (44.40) | 5238M | 93/93 | 0/152 | 10.09 (10.08) | 98M | 93/93 | 4.5x | 4.4x | pass |
| Issue039-bug-design.dsn | finished | 1 | 10/25 | 200.74 (202.23) | 6253M | 74/74 | 30/25 | 284.34 (284.33) | 108M | 70/74 | 0.7x | 0.7x | checker nets 70 < java 74 |
| Issue054-tairakb.dsn | finished | 1 | 5/5 | 111.58 (110.29) | 2403M | 55/56 | 0/5 | 4.66 (4.65) | 35M | 55/56 | 24.0x | 23.7x | pass |
| Issue093-interf_u.dsn | finished | 1 | 19/192 | 17.45 (14.58) | 5648M | 110/110 | 0/192 | 0.78 (0.77) | 15M | 110/110 | 22.3x | 18.9x | pass |
| Issue102-Mars-64-revE-rot00.dsn | finished | 1 | 0/64 | 88.35 (88.17) | 6153M | 74/74 | 1/64 | 126.38 (126.37) | 73M | 74/74 | 0.7x | 0.7x | pass |
| Issue103-Board-Routed.dsn | finished | 1 | 0/63 | 5.29 (2.13) | 5668M | 267/267 | 0/63 | 0.75 (0.74) | 21M | 267/267 | 7.0x | 2.9x | pass |
| Issue107-freq_teiler_200kHz_kicad.dsn | finished | 1 | 0/0 | 6.81 (3.92) | 6165M | 59/59 | 0/0 | 0.99 (0.98) | 18M | 59/59 | 6.9x | 4.0x | pass |
| Issue107-freq_teiler_200kHz_kicad_bad.dsn | finished | 1 | 0/0 | 6.81 (4.10) | 6160M | 59/59 | 0/0 | 1.14 (1.13) | 19M | 59/59 | 6.0x | 3.6x | pass |
| Issue110-Pajalnaja_stancija.dsn | finished | 1 | 6/18 | 64.59 (61.94) | 6923M | 39/41 | 6/18 | 97.89 (97.88) | 262M | 39/41 | 0.7x | 0.6x | pass |
| Issue110-RelayModule.dsn | finished | 1 | 0/3 | 9.88 (8.06) | 6146M | 44/44 | 0/4 | 15.50 (15.49) | 53M | 44/44 | 0.6x | 0.5x | pass |
| Issue110-testPCBSpecctraFile.dsn | finished | 1 | 21/41 | 29.64 (34.42) | 6116M | 24/25 | 3/41 | 8.27 (8.26) | 50M | 24/25 | 3.6x | 4.2x | pass |
| Issue110-testProjectFromFreeroutingBugTest.dsn | finished | 1 | 0/0 | 3.31 (0.48) | 721M | 4/4 | 0/0 | 0.03 (0.02) | 6M | 4/4 | 106.7x | 24.0x | pass |
| Issue110-testProjectFromFreeroutingBugTest01.dsn | finished | 1 | 0/0 | 3.26 (0.49) | 732M | 4/4 | 0/0 | 0.03 (0.02) | 6M | 4/4 | 105.3x | 24.5x | pass |
| Issue110-Паяльная станция.dsn | finished | 1 | 6/18 | 67.13 (64.03) | 6936M | 39/41 | 6/18 | 97.46 (97.45) | 269M | 39/41 | 0.7x | 0.7x | byte-identical duplicate of Issue110-Pajalnaja_stancija.dsn |
| Issue143-rpi_splitter.dsn | finished | 1 | 0/0 | 3.25 (0.56) | 916M | 4/4 | 0/0 | 0.06 (0.05) | 6M | 4/4 | 59.1x | 11.2x | pass |
| Issue143-rpi_splitter_mod.dsn | finished | 1 | 0/0 | 2.71 (0.22) | 422M | 1/4 | 0/0 | 0.02 (0.01) | 5M | 1/4 | 169.5x | 22.0x | pass |
| Issue153-wavefolder.dsn | finished | 1 | 3/9 | 6.31 (3.45) | 6138M | 28/30 | 0/9 | 1.02 (1.01) | 15M | 29/30 | 6.2x | 3.4x | pass |
| Issue155-CH376_MCP795_Module.dsn | finished | 1 | 0/0 | 5.31 (3.10) | 6961M | 25/25 | 0/0 | 2.52 (2.51) | 17M | 25/25 | 2.1x | 1.2x | pass |
| Issue157-TeamAdapt-LinePCB.dsn | finished | 1 | 14/18 | 81.32 (84.67) | 4920M | 64/64 | 2/18 | 76.76 (76.75) | 311M | 64/64 | 1.1x | 1.1x | pass |
| Issue159-setonix_2hp-pcb.dsn | finished | 1 | 0/0 | 3.27 (0.71) | 1387M | 6/6 | 0/0 | 0.24 (0.23) | 10M | 6/6 | 13.4x | 3.1x | pass |
| Issue163-pic_programmer.dsn | finished | 1 | 9/6 | 8.38 (5.38) | 6065M | 33/34 | 0/6 | 3.21 (3.20) | 41M | 33/34 | 2.6x | 1.7x | pass |
| Issue187-processor.Z80.dsn | finished | 1 | 0/0 | 4.84 (1.01) | 5667M | 0/245 | 0/0 | 1.05 (1.04) | 34M | 245/245 | 4.6x | 1.0x | pass |
| Issue209-split10.dsn | finished | 1 | 2/182 | 24.57 (22.34) | 5899M | 40/45 | 0/182 | 3.32 (3.31) | 27M | 40/45 | 7.4x | 6.7x | pass |
| Issue217-8088sbc.dsn | finished | 1 | 7/0 | 20.50 (17.79) | 6017M | 71/76 | 0/0 | 10.79 (10.78) | 58M | 76/76 | 1.9x | 1.7x | pass |
| Issue230-CNH_Functional_Tester_1.dsn | finished | 1 | 2/16 | 31.62 (28.90) | 4296M | 61/64 | 0/16 | 11.65 (11.64) | 153M | 62/64 | 2.7x | 2.5x | pass |

## Summary

- **java**: 27/27 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 117, violations 812; total wall 885.9s; checker completion 1387/1661 nets (83.5%)
- **rust**: 27/27 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 48, violations 813; total wall 770.9s; checker completion 1639/1661 nets (98.7%)
- **Fixture classes** (Java reference): java_finished 27
- **Quality filter** `placeholder-nets-v0`: 25/27 fixtures pass; 1 byte-identical duplicate(s) excluded from the filtered aggregate
- **Geomean speedup (java/rust), quality-filtered, serial (W=1)**: wall-clock 6.5x (n=25), in-tool 3.6x (n=25)
- Quality-filtered set: Issue026-J2_reference.dsn, Issue034-Green14SegLED.dsn, Issue035-ReadPlaceScope.dsn, Issue054-tairakb.dsn, Issue093-interf_u.dsn, Issue102-Mars-64-revE-rot00.dsn, Issue103-Board-Routed.dsn, Issue107-freq_teiler_200kHz_kicad.dsn, Issue107-freq_teiler_200kHz_kicad_bad.dsn, Issue110-Pajalnaja_stancija.dsn, Issue110-RelayModule.dsn, Issue110-testPCBSpecctraFile.dsn, Issue110-testProjectFromFreeroutingBugTest.dsn, Issue110-testProjectFromFreeroutingBugTest01.dsn, Issue143-rpi_splitter.dsn, Issue143-rpi_splitter_mod.dsn, Issue153-wavefolder.dsn, Issue155-CH376_MCP795_Module.dsn, Issue157-TeamAdapt-LinePCB.dsn, Issue159-setonix_2hp-pcb.dsn, Issue163-pic_programmer.dsn, Issue187-processor.Z80.dsn, Issue209-split10.dsn, Issue217-8088sbc.dsn, Issue230-CNH_Functional_Tester_1.dsn
- **quality-unfiltered, not comparable** (every serially timed fixture both tools finished, no quality check whatsoever — includes boards where we route a fraction of the nets): wall-clock 5.5x (n=27), in-tool 3.2x (n=27)

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction. Every ratio above is from `W=1` rows only; a sharded sweep produces quality verdicts and no speed number at all.
