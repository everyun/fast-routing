# bench/ — Freerouting (Java) vs fr-cli (Rust) benchmark harness

This is the project's referee: a head-to-head comparison of the reference
Java Freerouting against our Rust `fr-cli` over the upstream fixture suite.
Every future optimization gets judged by it. Python 3, stdlib only.

## Quick start

```sh
# 1. Build our CLI
cargo build --release -p fr-cli

# 2. Run the full top-level suite (46 fixtures x 2 tools, 20 passes, 300s timeout each)
python3 bench/run_bench.py

# Smoke subset / variations
python3 bench/run_bench.py --fixtures 'Issue026*' --fixtures 'Issue143*'
python3 bench/run_bench.py --only-rust --passes 5
python3 bench/run_bench.py --timeout 600
```

Results land in `bench/results/<timestamp>/`:

- `results.json` — one record per (tool, fixture), written incrementally so a
  partial/aborted suite still leaves data
- `report.md` — comparison table + summary (totals, geometric-mean speedup,
  completion rates)
- `logs/` — raw stdout/stderr per run (includes the `/usr/bin/time -l` footer)
- `ses/` — the produced `.ses` session files

Defaults (all overridable): fixtures `/tmp/freerouting-upstream/fixtures/*.dsn`
(top level only), Java `/opt/homebrew/opt/openjdk/bin/java`, jar
`/tmp/frbase/freerouting-2.3.0.jar`, Rust binary `target/release/fr-cli`.

## Metrics

| field | meaning |
|---|---|
| `wall_s` | whole-process wall clock, measured by the harness. For Java this **includes ~9-11s of JVM startup + board init**, which dominates small boards. |
| `tool_time_s` | tool-reported routing time. Java: sum of the "… stage completed: … completed in X seconds" lines (fanout + auto-routing + optimizer). Rust: the "Routing complete in Xs" line. This is the fairer algorithmic comparison. |
| `tool_cpu_s` (Java) | sum of self-reported "total CPU seconds" per stage. |
| `time_user_s` / `time_sys_s` | process CPU from `/usr/bin/time -l`. |
| `max_rss_bytes` | peak resident set from `/usr/bin/time -l`. On this macOS the value is in **bytes** (verified empirically: a JVM run reports ~4.2e9). |
| `peak_heap_bytes` (Java) | max self-reported "peak heap usage" across stages (JVM heap only, smaller than RSS). |
| `unrouted` / `violations` | self-reported. Java: from the last "final score: S (N unrouted and M violations)" stage line, falling back to the last per-pass line. Rust: derived from "N/M nets routed" and "DRC violations: D". |
| `score` (Java) | Freerouting's own board score (999.99 = clean). |
| `exit_code`, `timed_out`, `crashed` | `crashed` = killed by a signal, unexpected exit code, **or** no parseable completion line. Note: `fr-cli` exits 2 when routing finished with incompletes — that is a result, not a crash. Java can crash internally (stack trace mid-log) and still **exit 0 with a 0-byte SES**, which is why crash detection is parse-based. |
| `ses_bytes` | size of the produced session file. |
| `check` | independent coarse verification from `check_ses.py` (below). |

## Independent check (`check_ses.py`)

Neither tool's self-report is trusted blindly. `check_ses.py` parses the input
`.dsn` and output `.ses` with a small tolerant S-expression parser and reports:

- `dsn_nets_total` / `dsn_nets_multi_pin` — nets declared in the DSN
  `(network …)` section; the multi-pin subset (≥2 pins) is what actually
  needs routing
- `ses_nets_routed` — nets in the SES `(network_out …)` with ≥1 wire or via
- `wire_length_mm` — total polyline wire length (converted via the SES
  `(resolution …)`) 
- `via_count`

Standalone use: `python3 bench/check_ses.py <in.dsn> <out.ses>`

**Caveats:** this is a coarse completion metric. It does **not** verify
electrical connectivity (a single stray wire stub counts as "routed") nor
clearance/DRC. A proper Rust connectivity + clearance checker comes later.
It has already caught real discrepancies — e.g. fr-cli claiming `14/24 nets
routed` on a board whose SES contains routing for only 3 nets. Also, a tool
can legitimately report 0 unrouted while the checker shows `n-1/n`: some
multi-pin "nets" need no wires (pins already touching / connected by plane).

## Parsing / environment gotchas (learned the hard way)

- `(string_quote ")` appears in 43 fixtures and in both tools' SES output —
  a literal, unbalanced quote character that breaks naive tokenizers. The
  parser strips these directives before tokenizing.
- Java logs are forced to English with `-l en` (verified working); parsers
  still fall back to the bare `"(N unrouted and M violations)"` fragment and
  accept `,` as decimal separator in case of localization.
- Fixture names may contain spaces and non-ASCII (e.g.
  `Issue110-Паяльная станция.dsn`) — all subprocess calls use argv lists,
  never shell strings.
- Timeouts kill the whole process group (`start_new_session` + `killpg`),
  otherwise the JVM survives the death of its `/usr/bin/time` wrapper.
