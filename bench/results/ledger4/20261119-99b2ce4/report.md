# Freerouting (Java) vs fr-cli (Rust) benchmark

- Run: 2026-11-19T16:25:42
- Config label: `ledger4-mp20-300s`
- Passes: 20, timeout: 300.0s
- Concurrency: W=1 (`--serial-perf`: timings are quotable)
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `./target/release/fr-cli`
- Repo rev: `99b2ce4`
- Manifest: `bench/fixtures-manifest.json` (generated 2026-09-04T21:56:39Z)
- Quality predicate: `placeholder-nets-v0`

Cells: `class` = manifest classification of the **Java** reference on this board; `u/v` = unrouted/violations **as the tool reports them about itself** (no independent verification — fr-check, ROADMAP M1.4, replaces this column); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `nets(chk)` = check_ses.py, nets-with-routing / multi-pin nets in the DSN (coarse: no connectivity, no clearance); `quality` = the run's quality predicate — only `pass` rows feed the headline geomean; `W` = how many bench processes were running at once when this row was timed (`?` = a run recorded before the field existed). **Every timing on a `W > 1` row is inflated by contention and is excluded from the geomean whatever `quality` says** — see `--serial-perf`.

| fixture | class | W | java u/v (self) | java time s | java RSS | java nets(chk) | rust u/v (self) | rust time s | rust RSS | rust nets(chk) | wall J/R | in-tool J/R | quality |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Issue026-J2_reference.dsn | finished | 1 | 0/0 | 3.77 (1.45) | 2901M | 12/12 | 0/0 | 0.14 (0.13) | 8M | 12/12 | 27.7x | 11.2x | pass |
| Issue034-Green14SegLED.dsn | finished | 1 | 9/0 | 27.07 (26.27) | 6021M | 97/101 | 0/0 | 3.51 (3.50) | 48M | 101/101 | 7.7x | 7.5x | pass |
| Issue035-ReadPlaceScope.dsn | finished | 1 | 4/152 | 40.24 (39.02) | 6173M | 93/93 | 0/152 | 2.62 (2.61) | 96M | 93/93 | 15.4x | 15.0x | pass |
| Issue039-bug-design.dsn | finished | 1 | 10/25 | 179.22 (180.32) | 6423M | 74/74 | 4/25 | 130.84 (130.83) | 565M | 74/74 | 1.4x | 1.4x | pass |
| Issue054-tairakb.dsn | finished | 1 | 5/5 | 93.52 (92.24) | 4615M | 55/56 | 0/5 | 1.55 (1.54) | 50M | 55/56 | 60.4x | 59.9x | pass |
| Issue093-interf_u.dsn | finished | 1 | 19/192 | 16.93 (13.87) | 5651M | 110/110 | 0/192 | 0.34 (0.34) | 20M | 110/110 | 49.4x | 40.8x | pass |
| Issue102-Mars-64-revE-rot00.dsn | finished | 1 | 0/64 | 85.43 (85.31) | 6173M | 74/74 | 0/64 | 38.69 (38.68) | 237M | 74/74 | 2.2x | 2.2x | pass |
| Issue103-Board-Routed.dsn | finished | 1 | 0/63 | 4.76 (1.92) | 5678M | 267/267 | 0/63 | 0.21 (0.20) | 26M | 267/267 | 22.9x | 9.6x | pass |
| Issue107-freq_teiler_200kHz_kicad.dsn | finished | 1 | 0/0 | 6.29 (3.64) | 6158M | 59/59 | 0/0 | 0.43 (0.42) | 18M | 59/59 | 14.7x | 8.7x | pass |
| Issue107-freq_teiler_200kHz_kicad_bad.dsn | finished | 1 | 0/0 | 6.78 (3.77) | 6176M | 59/59 | 0/0 | 0.48 (0.48) | 18M | 59/59 | 14.0x | 7.9x | pass |
| Issue110-Pajalnaja_stancija.dsn | finished | 1 | 6/18 | 61.52 (58.80) | 6956M | 39/41 | 6/18 | 30.23 (30.23) | 91M | 39/41 | 2.0x | 1.9x | pass |
| Issue110-RelayModule.dsn | finished | 1 | 0/3 | 9.32 (7.30) | 6051M | 44/44 | 0/4 | 4.05 (4.04) | 65M | 44/44 | 2.3x | 1.8x | pass |
| Issue110-testPCBSpecctraFile.dsn | finished | 1 | 21/41 | 27.61 (31.68) | 6072M | 24/25 | 3/41 | 4.10 (4.09) | 82M | 24/25 | 6.7x | 7.7x | pass |
| Issue110-testProjectFromFreeroutingBugTest.dsn | finished | 1 | 0/0 | 3.18 (0.41) | 731M | 4/4 | 0/0 | 0.02 (0.01) | 6M | 4/4 | 159.0x | 41.0x | pass |
| Issue110-testProjectFromFreeroutingBugTest01.dsn | finished | 1 | 0/0 | 3.20 (0.43) | 733M | 4/4 | 0/0 | 0.02 (0.01) | 6M | 4/4 | 188.0x | 43.0x | pass |
| Issue110-Паяльная станция.dsn | finished | 1 | 6/18 | 61.56 (59.01) | 6930M | 39/41 | 6/18 | 30.25 (30.24) | 254M | 39/41 | 2.0x | 2.0x | byte-identical duplicate of Issue110-Pajalnaja_stancija.dsn |
| Issue143-rpi_splitter.dsn | finished | 1 | 0/0 | 3.19 (0.54) | 910M | 4/4 | 0/0 | 0.03 (0.02) | 7M | 4/4 | 106.4x | 27.0x | pass |
| Issue143-rpi_splitter_mod.dsn | finished | 1 | 0/0 | 2.68 (0.20) | 420M | 1/4 | 0/0 | 0.02 (0.01) | 6M | 1/4 | 167.8x | 20.0x | pass |
| Issue153-wavefolder.dsn | finished | 1 | 3/9 | 6.30 (3.27) | 6139M | 28/30 | 0/9 | 0.45 (0.44) | 21M | 29/30 | 14.0x | 7.4x | pass |
| Issue155-CH376_MCP795_Module.dsn | finished | 1 | 0/0 | 5.26 (2.79) | 6915M | 25/25 | 0/0 | 0.56 (0.55) | 27M | 25/25 | 9.4x | 5.1x | pass |
| Issue157-TeamAdapt-LinePCB.dsn | finished | 1 | 14/18 | 77.84 (80.84) | 5659M | 64/64 | 2/18 | 26.81 (26.80) | 385M | 64/64 | 2.9x | 3.0x | pass |
| Issue159-setonix_2hp-pcb.dsn | finished | 1 | 0/0 | 3.21 (0.63) | 1409M | 6/6 | 0/0 | 0.10 (0.09) | 11M | 6/6 | 33.4x | 7.0x | pass |
| Issue163-pic_programmer.dsn | finished | 1 | 9/6 | 7.79 (4.88) | 6072M | 33/34 | 0/6 | 2.08 (2.08) | 65M | 33/34 | 3.7x | 2.3x | pass |
| Issue187-processor.Z80.dsn | finished | 1 | 0/0 | 4.27 (0.88) | 5639M | 0/245 | 0/0 | 0.30 (0.29) | 34M | 245/245 | 14.3x | 3.0x | pass |
| Issue209-split10.dsn | finished | 1 | 2/182 | 23.02 (21.14) | 5906M | 40/45 | 0/182 | 2.16 (2.15) | 53M | 40/45 | 10.7x | 9.8x | pass |
| Issue217-8088sbc.dsn | finished | 1 | 7/0 | 20.02 (17.54) | 6146M | 71/76 | 0/0 | 3.37 (3.36) | 63M | 76/76 | 5.9x | 5.2x | pass |
| Issue230-CNH_Functional_Tester_1.dsn | finished | 1 | 2/16 | 30.67 (27.83) | 4280M | 61/64 | 0/16 | 6.64 (6.63) | 149M | 62/64 | 4.6x | 4.2x | pass |

## Summary

- **java**: 27/27 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 117, violations 812; total wall 814.7s; checker completion 1387/1661 nets (83.5%)
- **rust**: 27/27 runs finished (0 timeouts, 0 crashes); self-reported total unrouted 21, violations 813; total wall 290.0s; checker completion 1643/1661 nets (98.9%)
- **Fixture classes** (Java reference): java_finished 27
- **Quality filter** `placeholder-nets-v0`: 26/27 fixtures pass; 1 byte-identical duplicate(s) excluded from the filtered aggregate
- **Geomean speedup (java/rust), quality-filtered, serial (W=1)**: wall-clock 13.8x (n=26), in-tool 7.9x (n=26)
- Quality-filtered set: Issue026-J2_reference.dsn, Issue034-Green14SegLED.dsn, Issue035-ReadPlaceScope.dsn, Issue039-bug-design.dsn, Issue054-tairakb.dsn, Issue093-interf_u.dsn, Issue102-Mars-64-revE-rot00.dsn, Issue103-Board-Routed.dsn, Issue107-freq_teiler_200kHz_kicad.dsn, Issue107-freq_teiler_200kHz_kicad_bad.dsn, Issue110-Pajalnaja_stancija.dsn, Issue110-RelayModule.dsn, Issue110-testPCBSpecctraFile.dsn, Issue110-testProjectFromFreeroutingBugTest.dsn, Issue110-testProjectFromFreeroutingBugTest01.dsn, Issue143-rpi_splitter.dsn, Issue143-rpi_splitter_mod.dsn, Issue153-wavefolder.dsn, Issue155-CH376_MCP795_Module.dsn, Issue157-TeamAdapt-LinePCB.dsn, Issue159-setonix_2hp-pcb.dsn, Issue163-pic_programmer.dsn, Issue187-processor.Z80.dsn, Issue209-split10.dsn, Issue217-8088sbc.dsn, Issue230-CNH_Functional_Tester_1.dsn
- **quality-unfiltered, not comparable** (every serially timed fixture both tools finished, no quality check whatsoever — includes boards where we route a fraction of the nets): wall-clock 12.9x (n=27), in-tool 7.5x (n=27)

> Wall-clock includes JVM startup (~10 s) which dominates small boards; in-tool time is the fairer algorithmic comparison and the product headline stays wall-clock with startup broken out (ROADMAP G4). Both `u/v` columns are self-reports and both `nets(chk)` columns are coarse completion counts — neither is a DRC verdict, so the quality filter above is a floor, not a gate. `degenerate` = Java finished with score 0.00 / no nets to route; those boards look like clean wins and are excluded by construction. Every ratio above is from `W=1` rows only; a sharded sweep produces quality verdicts and no speed number at all.
