# Freerouting (Java) vs fr-cli (Rust) benchmark

- Date: 2026-08-28T03:17:06
- Passes: 20, timeout: 300.0s
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/Users/chiqingzhen/Documents/fast-routing/target/release/fr-cli`
- Repo rev: `67f5256`

Cells: `u/v` = unrouted/violations (self-reported); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `chk` = independent checker, nets-with-routing / multi-pin nets in DSN (coarse, no connectivity verification).

| fixture | java u/v | java time s | java RSS | java chk | rust u/v | rust time s | rust RSS | rust chk | wall speedup J/R |
|---|---|---|---|---|---|---|---|---|---|
| Issue006-LPC18XX_43XX_SCH.dsn | 0/0 | 13.92 (3.78) | 2041M | 0/0 | CRASH | 0.01 | 3M | - | - |
| Issue015-StackOverflow.dsn | TIMEOUT | - | - | - | 146/0 | 5.62 (5.61) | 576M | 182/186 | - |
| Issue022-AutoRouter_interrupted.dsn | TIMEOUT | - | - | - | 149/0 | 2.00 (1.99) | 538M | 198/204 | - |
| Issue026-J2_reference.dsn | 0/0 | 10.64 (2.08) | 4438M | 12/12 | 10/0 | 0.01 (0.01) | 18M | 3/12 | 759.9x |
| Issue027-zMRETestFixture.dsn | TIMEOUT | - | - | - | 64/0 | 2.58 (2.57) | 425M | 166/168 | - |
| Issue029-hw48na.dsn | TIMEOUT | - | - | - | 104/0 | 8.06 (8.05) | 579M | 147/163 | - |
| Issue034-Green14SegLED.dsn | 3/0 | 29.17 (21.59) | 4124M | 99/101 | 92/0 | 0.57 (0.56) | 298M | 18/101 | 51.1x |
| Issue035-ReadPlaceScope.dsn | 6/152 | 25.83 (17.38) | 11084M | 93/93 | 29/0 | 1.70 (1.69) | 421M | 84/93 | 15.2x |
| Issue039-bug-design.dsn | 9/25 | 166.38 (160.86) | 4566M | 74/74 | 72/0 | 1.35 (1.34) | 428M | 23/74 | 123.4x |
| Issue054-tairakb.dsn | 1/5 | 80.19 (71.83) | 4091M | 55/56 | 24/0 | 2.07 (2.06) | 355M | 48/56 | 38.8x |
| Issue066-Project_GP8B.dsn | TIMEOUT | - | - | - | 206/0 | 16.83 (16.81) | 624M | 40/229 | - |
| Issue070-Autorouter_FQ101_PCB_2022-05-13.dsn | TIMEOUT | - | - | - | 67/27 | 0.01 (0.01) | 8M | 70/69 | - |
| Issue093-interf_u.dsn | 18/192 | 23.09 (13.63) | 10943M | 110/110 | 105/0 | 0.44 (0.43) | 398M | 110/110 | 52.5x |
| Issue102-Mars-64-revE-rot00.dsn | 1/64 | 53.71 (46.63) | 3845M | 74/74 | 72/0 | 0.53 (0.52) | 322M | 70/74 | 100.8x |
| Issue103-Board-Routed.dsn | 0/63 | 12.46 (2.22) | 11013M | 267/267 | 261/4 | 98.13 (98.11) | 383M | 186/267 | 0.1x |
| Issue103-Board-Unrouted.dsn | TIMEOUT | - | - | - | 259/12 | 5.89 (5.88) | 506M | 26/267 | - |
| Issue107-freq_teiler_200kHz_kicad.dsn | 2/0 | 14.09 (4.12) | 5157M | 59/59 | 39/0 | 0.41 (0.40) | 350M | 24/59 | 34.6x |
| Issue107-freq_teiler_200kHz_kicad_bad.dsn | TIMEOUT | - | - | - | 39/0 | 0.38 (0.36) | 358M | 24/59 | - |
| Issue110-Pajalnaja_stancija.dsn | 6/18 | 72.76 (65.89) | 3959M | 39/41 | 38/0 | 0.82 (0.81) | 370M | 15/41 | 89.1x |
| Issue110-RelayModule.dsn | 0/3 | 20.33 (12.17) | 4053M | 44/44 | 40/1 | 2.56 (2.55) | 398M | 22/44 | 7.9x |
| Issue110-testPCBSpecctraFile.dsn | 20/41 | 32.50 (29.57) | 4024M | 24/25 | 19/0 | 0.56 (0.55) | 273M | 24/25 | 58.3x |
| Issue110-testProjectFromFreeroutingBugTest.dsn | 0/0 | 9.31 (0.48) | 761M | 4/4 | 4/0 | 0.07 (0.06) | 19M | 0/4 | 131.2x |
| Issue110-testProjectFromFreeroutingBugTest01.dsn | 0/0 | 9.43 (0.50) | 784M | 4/4 | 4/0 | 0.07 (0.06) | 18M | 0/4 | 136.7x |
| Issue110-Паяльная станция.dsn | 6/18 | 71.89 (62.83) | 3708M | 39/41 | 38/0 | 0.81 (0.80) | 382M | 15/41 | 89.0x |
| Issue113-Protein.dsn | TIMEOUT | - | - | - | 154/0 | 5.91 (5.89) | 493M | 79/160 | - |
| Issue143-rpi_splitter.dsn | 0/0 | 8.68 (1.16) | 1629M | 4/5 | 4/0 | 0.01 (0.00) | 3M | 1/5 | 963.9x |
| Issue143-rpi_splitter_mod.dsn | 0/0 | 5.49 (0.25) | 471M | 1/5 | 5/0 | 0.06 (0.06) | 16M | 1/5 | 85.8x |
| Issue145-smoothieboard.dsn | TIMEOUT | - | - | - | 162/4 | 14.56 (14.54) | 681M | 138/225 | - |
| Issue153-wavefolder.dsn | 3/9 | 13.01 (3.59) | 4443M | 28/30 | 24/0 | 0.57 (0.56) | 230M | 23/30 | 22.9x |
| Issue155-CH376_MCP795_Module.dsn | 0/0 | 14.78 (3.76) | 4085M | 25/25 | 23/0 | 0.50 (0.49) | 232M | 3/25 | 29.8x |
| Issue157-TeamAdapt-LinePCB.dsn | 15/18 | 92.60 (89.78) | 4305M | 64/64 | 36/0 | 3.73 (3.72) | 355M | 60/64 | 24.8x |
| Issue159-setonix_2hp-pcb.dsn | 0/0 | 9.93 (0.78) | 1536M | 6/6 | 5/0 | 0.25 (0.25) | 72M | 4/6 | 39.1x |
| Issue163-pic_programmer.dsn | 9/6 | 14.64 (5.13) | 10509M | 33/34 | 24/0 | 0.58 (0.58) | 285M | 34/34 | 25.0x |
| Issue178-KeebMaker_Sofle_Choc.dsn | TIMEOUT | - | - | - | 115/5 | 5.44 (5.43) | 623M | 114/170 | - |
| Issue179-Autorouter_PCB1_2023-3-24.dsn | CRASH | 8.85 | 459M | 0/20 | 19/0 | 0.01 (0.00) | 4M | 1/20 | - |
| Issue187-processor.Z80.dsn | 0/0 | 11.84 (1.00) | 10960M | 245/245 | TIMEOUT | - | - | - | - |
| Issue190-processor.Z80.dsn | 12/1372 | 260.78 (250.38) | 5130M | 244/244 | 241/0 | 12.96 (12.94) | 690M | 244/244 | 20.1x |
| Issue208-freerouting.dsn | TIMEOUT | - | - | - | 298/0 | 0.74 (0.73) | 395M | 2/299 | - |
| Issue209-split05.dsn | 20/183 | 289.18 (280.60) | 3628M | 40/45 | 39/0 | 2.78 (2.77) | 545M | 38/45 | 104.1x |
| Issue209-split10.dsn | 1/183 | 29.10 (20.80) | 4062M | 40/45 | 39/0 | 2.80 (2.79) | 539M | 38/45 | 10.4x |
| Issue214-freerouting.dsn | TIMEOUT | - | - | - | 107/4 | 8.74 (8.72) | 630M | 129/129 | - |
| Issue217-8088sbc.dsn | 2/0 | 25.06 (16.01) | 7019M | 74/76 | 74/0 | 4.80 (4.79) | 304M | 5/76 | 5.2x |
| Issue219-LogicBoard_smt.dsn | TIMEOUT | - | - | - | 297/0 | 30.86 (30.85) | 736M | 47/317 | - |
| Issue229-display-8-digit-hc595.dsn | 0/0 | 8.94 (0.03) | 444M | 0/30 | 30/0 | 4.79 (4.78) | 264M | 30/30 | 1.9x |
| Issue230-CNH_Functional_Tester_1.dsn | 2/16 | 42.08 (33.00) | 3971M | 61/64 | 59/0 | 0.46 (0.45) | 586M | 10/64 | 91.9x |
| Issue269-caniot-tiny-arm.dsn | 92/4 | 160.92 (158.51) | 8409M | 38/76 | 69/0 | 3.78 (3.77) | 421M | 37/76 | 42.6x |

## Summary

- **java**: 31/46 fixtures finished (14 timeouts, 1 crashes); total unrouted 228, violations 2372; total wall 1632.7s; checker completion 1900/1999 nets (95.0%)
- **rust**: 44/46 fixtures finished (1 timeouts, 1 crashes); total unrouted 3705, violations 57; total wall 255.8s; checker completion 2533/4419 nets (57.3%)
- **Geometric-mean speedup (rust vs java)**: wall-clock 37.6x, tool-reported 12.3x

> Wall-clock includes JVM startup (~10s+) which dominates small boards; the tool-reported number is the fairer algorithmic comparison. Checker completion is coarse — see check_ses.py caveats.
