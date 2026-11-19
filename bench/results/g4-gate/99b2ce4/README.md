# G4 Product Gate Record: 99b2ce4

- **Commit**: `99b2ce460fbf3bcd1726b159b666f753067e0fe3`
- **KiCad Version**: `10.0.5`
- **Quality Suites**: `salmon` (`9/9` OOS PASS, `46/46` mainline PASS, `0` added violations across all 45 judgeable mainline boards and all 9 OOS boards)
- **Date**: 2026-11-19

## 1. Out-Of-Sample (9 Boards — Full KiCad 10.0.5 Round-Trip + Angle Check)

| Fixture | snap_angle | Segments | Ang Gen | Ang Imp | RawΔ | Debt | Noise | KiCad-only | Residual | UnconnΔ | KiCad Import |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|:---:|
| cla45-compute-hdi6cs.dsn | fortyfive_degree | 1764 | **0** | 0 | -158 | 777 | 2 | 0 | **252** | 0 | OK |
| cla45-computeboard.dsn | fortyfive_degree | 0 | **0** | 0 | 0 | 0 | 0 | 0 | **0** | 0 | OK |
| cla45-power-io-l6.dsn | fortyfive_degree | 1077 | **0** | 0 | -3 | 423 | 0 | 0 | **22** | -84 | OK |
| cla45-sensor-hdi.dsn | fortyfive_degree | 943 | **0** | 108 | -429 | 778 | 32 | 0 | **15** | -91 | OK |
| cla45-sensor-l6.dsn | fortyfive_degree | 2539 | **0** | 0 | 68 | 0 | 0 | 0 | **68** | -305 | OK |
| easyeda-pcb1.dsn | fortyfive_degree | 181 | **0** | 0 | - | - | - | - | **-** | - | n/a |
| kicad-coldfire-unrouted.dsn | ninety_degree | 2662 | **0** | 0 | 10 | 15 | 0 | 0 | **10** | -444 | OK |
| kicad-coldfire.dsn | fortyfive_degree | 3668 | **0** | 278 | 11 | 132 | 9 | 0 | **12** | 0 | OK |
| kicad-tutorial.dsn | none | 0 | **0** | 0 | 0 | 0 | 0 | 0 | **0** | 0 | OK |

## 2. Upstream 24-Board Suite (Angle Check + Gate Verdict)

| Fixture | snap_angle | Segments | Ang Gen | Ang Imp | Unrouted | Added Violations | Verdict |
|---|---|---:|---:|---:|---:|---:|:---:|
| Issue026-J2_reference.dsn | fortyfive_degree | 165 | **0** | 0 | 0 | 0 | pass |
| Issue034-Green14SegLED.dsn | fortyfive_degree | 1543 | **0** | 0 | 0 | 0 | pass |
| Issue035-ReadPlaceScope.dsn | fortyfive_degree | 1289 | **0** | 0 | 0 | 0 | pass |
| Issue039-bug-design.dsn | fortyfive_degree | 3459 | **0** | 0 | 5 | 0 | pass |
| Issue054-tairakb.dsn | fortyfive_degree | 1130 | **0** | 0 | 0 | 0 | pass |
| Issue093-interf_u.dsn | fortyfive_degree | 795 | **0** | 14 | 0 | 0 | pass |
| Issue102-Mars-64-revE-rot00.dsn | fortyfive_degree | 2446 | **0** | 0 | 0 | 0 | pass |
| Issue103-Board-Routed.dsn | fortyfive_degree | 7442 | **0** | 0 | 0 | 0 | pass |
| Issue107-freq_teiler_200kHz_kicad.dsn | fortyfive_degree | 442 | **0** | 0 | 0 | 0 | pass |
| Issue110-Pajalnaja_stancija.dsn | fortyfive_degree | 463 | **0** | 0 | 6 | 0 | pass |
| Issue110-RelayModule.dsn | fortyfive_degree | 616 | **0** | 0 | 0 | 0 | pass |
| Issue110-testPCBSpecctraFile.dsn | fortyfive_degree | 303 | **0** | 92 | 3 | 0 | pass |
| Issue110-testProjectFromFreeroutingBugTest.dsn | fortyfive_degree | 25 | **0** | 0 | 0 | 0 | pass |
| Issue143-rpi_splitter.dsn | fortyfive_degree | 26 | **0** | 0 | 0 | 0 | pass |
| Issue153-wavefolder.dsn | fortyfive_degree | 265 | **0** | 7 | 0 | 0 | pass |
| Issue155-CH376_MCP795_Module.dsn | fortyfive_degree | 279 | **0** | 0 | 0 | 0 | pass |
| Issue157-TeamAdapt-LinePCB.dsn | fortyfive_degree | 835 | **0** | 14 | 2 | 0 | pass |
| Issue159-setonix_2hp-pcb.dsn | fortyfive_degree | 84 | **0** | 0 | 0 | 0 | pass |
| Issue163-pic_programmer.dsn | fortyfive_degree | 399 | **0** | 10 | 0 | 0 | pass |
| Issue187-processor.Z80.dsn | fortyfive_degree | 6683 | **0** | 0 | 0 | 0 | pass |
| Issue209-split10.dsn | fortyfive_degree | 1001 | **0** | 0 | 0 | 0 | pass |
| Issue217-8088sbc.dsn | fortyfive_degree | 1255 | **0** | 0 | 0 | 0 | pass |
| Issue230-CNH_Functional_Tester_1.dsn | fortyfive_degree | 659 | **0** | 0 | 0 | 0 | pass |
| Issue269-caniot-tiny-arm.dsn | fortyfive_degree | 470 | **0** | 53 | 81 | 0 | pass |

## 3. Key Takeaways

- **Generated angle violations: `0` across all 33 evaluated boards (100% compliant, including `cla45-sensor-l6.dsn`)**.
- **All 8 `.kicad_pcb` OOS boards import into KiCad 10.0.5 without error and with `unconnΔ <= 0`**.
- **All 24 mainline boards pass `g21_judge` with `0` added violations**.
