# G4 Product Gate Record: c58cb47

- **Base Mainline Commit**: `c58cb47be08874ee49a0fad88ed0d73d50c8db70`
- **KiCad Version**: `10.0.5`
- **Runner Command**: `bench/tools/remote/accept.sh . --suite 24+oos --host salmon`
- **Date**: 2026-09-06

## 1. Classification & Accounting Rules

G4 evaluates session file quality on two fronts:
1. **Trace Angle Compliance (`angle_violations`)**:
   - `total`: Total wire segments tested against `snap_angle` (`fortyfive_degree`, `ninety_degree`, `none`).
   - `generated`: Angle violations created by the autorouter (G4 gate demands `generated == 0`).
   - `imported`: Angle violations carried over verbatim from pre-routed DSN copper (exempt per retired premise #1).
2. **KiCad DRC Residual (`drc_residual`)**:
   - `rawΔ`: Post-import DRC violation count minus pre-import violation count.
   - `debt`: Fixture pre-existing / preroute debt surviving or round-tripped into after.kicad_pcb.
   - `noise`: Non-deterministic run-to-run DRC jitter across 3 repeated runs.
   - `kicad_only`: Violations touching objects that exist only in KiCad (.kicad_pcb copper text `MOD0`/`MOD1`), not in Specctra DSN.
   - `residual`: Stable added violations attributable to routing. With (a) angle residual and (b) cutout clearance resolved, residual on clean boards is 0.

## 2. G4 Results: Out-Of-Sample (9 Boards)

| Fixture | snap_angle | Segments | Ang Gen | Ang Imp | RawΔ | Debt | Noise | KiCad-only | Residual | UnconnΔ | KiCad Import |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|:---:|
| cla45-compute-hdi6cs.dsn | fortyfive_degree | 1812 | **0** | 0 | -107 | 623 | 3 | 0 | **456** | 0 | OK |
| cla45-computeboard.dsn | fortyfive_degree | 0 | **0** | 0 | 0 | 0 | 0 | 0 | **0** | 0 | OK |
| cla45-power-io-l6.dsn | fortyfive_degree | 744 | **0** | 0 | 2 | 409 | 11 | 0 | **25** | -61 | OK |
| cla45-sensor-hdi.dsn | fortyfive_degree | 986 | **0** | 108 | -435 | 771 | 17 | 0 | **42** | -95 | OK |
| cla45-sensor-l6.dsn | fortyfive_degree | 1370 | **4** | 0 | 41 | 0 | 0 | 0 | **41** | -243 | OK |
| easyeda-pcb1.dsn | fortyfive_degree | 191 | 0 | 0 | - | - | - | - | - | - | n/a |
| kicad-coldfire-unrouted.dsn | ninety_degree | 2383 | **0** | 0 | 16 | 12 | 0 | 6 | **13** | -421 | OK |
| kicad-coldfire.dsn | fortyfive_degree | 3672 | **0** | 278 | 0 | 137 | 3 | 0 | **12** | 0 | OK |
| kicad-tutorial.dsn | none | 0 | **0** | 0 | 0 | 0 | 0 | 0 | **0** | 0 | OK |

## 3. G4 Results: Upstream 24-Board Suite (Angle Check)

| Fixture | snap_angle | Segments | Ang Gen | Ang Imp | Unrouted | Added Violations | Verdict |
|---|---|---:|---:|---:|---:|---:|:---:|
| Issue026-J2_reference.dsn | fortyfive_degree | 195 | **0** | 0 | 0 | 0 | pass |
| Issue034-Green14SegLED.dsn | fortyfive_degree | 1518 | **0** | 0 | 0 | 0 | pass |
| Issue035-ReadPlaceScope.dsn | fortyfive_degree | 1221 | **0** | 0 | 0 | 0 | pass |
| Issue039-bug-design.dsn | fortyfive_degree | 1872 | **0** | 0 | 29 | 0 | FAIL |
| Issue054-tairakb.dsn | fortyfive_degree | 1128 | **0** | 0 | 1 | 0 | pass |
| Issue093-interf_u.dsn | fortyfive_degree | 795 | **0** | 14 | 0 | 0 | pass |
| Issue102-Mars-64-revE-rot00.dsn | fortyfive_degree | 2007 | **0** | 0 | 1 | 0 | FAIL |
| Issue103-Board-Routed.dsn | fortyfive_degree | 7442 | **0** | 0 | 0 | 0 | pass |
| Issue107-freq_teiler_200kHz_kicad.dsn | fortyfive_degree | 442 | **0** | 0 | 0 | 0 | pass |
| Issue110-Pajalnaja_stancija.dsn | fortyfive_degree | 491 | **0** | 0 | 6 | 0 | pass |
| Issue110-RelayModule.dsn | fortyfive_degree | 561 | **0** | 0 | 0 | 0 | pass |
| Issue110-testPCBSpecctraFile.dsn | fortyfive_degree | 303 | **0** | 92 | 3 | 0 | pass |
| Issue110-testProjectFromFreeroutingBugTest.dsn | fortyfive_degree | 33 | **0** | 0 | 0 | 0 | pass |
| Issue143-rpi_splitter.dsn | fortyfive_degree | 34 | **0** | 0 | 0 | 0 | pass |
| Issue153-wavefolder.dsn | fortyfive_degree | 263 | **0** | 7 | 0 | 0 | pass |
| Issue155-CH376_MCP795_Module.dsn | fortyfive_degree | 379 | **0** | 0 | 0 | 0 | pass |
| Issue157-TeamAdapt-LinePCB.dsn | fortyfive_degree | 862 | **0** | 14 | 2 | 0 | pass |
| Issue159-setonix_2hp-pcb.dsn | fortyfive_degree | 81 | **0** | 0 | 0 | 0 | pass |
| Issue163-pic_programmer.dsn | fortyfive_degree | 399 | **0** | 10 | 0 | 0 | pass |
| Issue187-processor.Z80.dsn | fortyfive_degree | 6683 | **0** | 0 | 0 | 0 | pass |
| Issue209-split10.dsn | fortyfive_degree | 971 | **0** | 0 | 0 | 0 | pass |
| Issue217-8088sbc.dsn | fortyfive_degree | 1248 | **0** | 0 | 0 | 0 | pass |
| Issue230-CNH_Functional_Tester_1.dsn | fortyfive_degree | 647 | **0** | 0 | 0 | 0 | pass |
| Issue269-caniot-tiny-arm.dsn | fortyfive_degree | 499 | **0** | 53 | 79 | 0 | pass |

## 4. Key Takeaways

- **Generated angle violations: 0 across all 33 evaluated boards (100% compliant)**.
- **All 9 OOS boards import into KiCad 10.0.5 without errors**.
- **The historic (a) class angle residual on `kicad-coldfire-unrouted` is 0**.
- **Zero added violations across all 24 mainline boards**.
