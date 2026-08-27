# Freerouting (Java) vs fr-cli (Rust) benchmark

- Date: 2026-08-28T01:27:29
- Passes: 20, timeout: 300.0s
- Jar: `/tmp/frbase/freerouting-2.3.0.jar`
- Rust binary: `/Users/chiqingzhen/Documents/fast-routing/target/release/fr-cli`
- Repo rev: `67f5256`

Cells: `u/v` = unrouted/violations (self-reported); `time` = wall s (tool-reported routing s in parens — for Java this excludes JVM startup); `RSS` = peak resident set; `chk` = independent checker, nets-with-routing / multi-pin nets in DSN (coarse, no connectivity verification).

| fixture | java u/v | java time s | java RSS | java chk | rust u/v | rust time s | rust RSS | rust chk | wall speedup J/R |
|---|---|---|---|---|---|---|---|---|---|
| Issue026-J2_reference.dsn | 0/0 | 10.86 (2.06) | 4041M | 12/12 | 10/0 | 0.49 (0.01) | 18M | 3/12 | 22.0x |
| Issue143-rpi_splitter.dsn | 0/0 | 10.01 (1.18) | 1611M | 4/5 | 4/0 | 0.01 (0.00) | 3M | 1/5 | 1429.6x |
| Issue179-Autorouter_PCB1_2023-3-24.dsn | CRASH | 9.07 | 511M | 0/20 | 19/0 | 0.01 (0.00) | 4M | 1/20 | - |

## Summary

- **java**: 2/3 fixtures finished (0 timeouts, 1 crashes); total unrouted 0, violations 0; total wall 20.9s; checker completion 16/17 nets (94.1%)
- **rust**: 3/3 fixtures finished (0 timeouts, 0 crashes); total unrouted 33, violations 0; total wall 0.5s; checker completion 5/37 nets (13.5%)
- **Geometric-mean speedup (rust vs java)**: wall-clock 177.3x, tool-reported 206.0x

> Wall-clock includes JVM startup (~10s+) which dominates small boards; the tool-reported number is the fairer algorithmic comparison. Checker completion is coarse — see check_ses.py caveats.
