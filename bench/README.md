# bench/ — Freerouting (Java) vs fr-cli (Rust) benchmark harness

This is the project's referee: a head-to-head comparison of the reference
Java Freerouting against our Rust `fr-cli` over the upstream fixture suite.
Every future optimization gets judged by it. Python 3, stdlib only.

Three tools:

| file | role |
|---|---|
| `manifest.py` → `fixtures-manifest.json` | what each fixture *is*: md5 dedup, coverage properties, Java classification |
| `run_bench.py` | runs both tools, classifies every row from the manifest, writes `results.json` + `report.md` |
| `check_ses.py` | independent (coarse) DSN/SES checker — totals **and per-net** wire length / via count |
| `test_bench.py` | self-tests for all three (`python3 bench/test_bench.py`); hermetic, no fixtures or jar needed |

## Quick start

```sh
# 1. Build our CLI
cargo build --release -p fr-cli

# 2. Generate the fixture manifest (row classification + the geomean need it)
python3 bench/manifest.py

# 3. Run the full top-level suite (46 fixtures x 2 tools, 20 passes, 300s timeout each)
python3 bench/run_bench.py

# Smoke subset / variations
python3 bench/run_bench.py --fixtures 'Issue026*' --fixtures 'Issue143*'
python3 bench/run_bench.py --only-rust --passes 5
python3 bench/run_bench.py --jar /tmp/fr-lab-release/freerouting-lab.jar \
    --passes 9999 --timeout 600 --config mp9999-lab

# Re-render an old run's report with today's report code (runs nothing)
python3 bench/run_bench.py --render-from bench/results/<stamp> --report-out -

# Harness self-tests (fast, hermetic) and manifest staleness check
python3 bench/test_bench.py
python3 bench/manifest.py --check
```

Results land in `bench/results/<timestamp>/`:

- `results.json` — `{"meta": {...}, "records": [...]}`, written incrementally
  so a partial/aborted suite still leaves data. `meta` pins the run's config
  (`config_label`, jar, passes, timeout, manifest, quality predicate, repo
  rev) so two runs made under different reference configs are never silently
  compared. Readers accept the older bare-list form too.
- `report.md` — comparison table + summary (per-row class, quality verdict,
  quality-filtered geomean)
- `logs/` — raw stdout/stderr per run (includes the `/usr/bin/time -l` footer)
- `ses/` — the produced `.ses` session files
- `nets/<fixture>.<tool>.nets.json` — the per-net breakdown from `check_ses.py`
  (kept out of `results.json`, which would otherwise be mostly net names)

Defaults (all overridable): fixtures `/tmp/freerouting-upstream/fixtures/*.dsn`
(top level only), Java `/opt/homebrew/opt/openjdk/bin/java`, jar
`/tmp/frbase/freerouting-2.3.0.jar`, Rust binary `target/release/fr-cli`,
manifest `bench/fixtures-manifest.json`.

## Fixture manifest (`manifest.py` → `fixtures-manifest.json`)

The manifest answers three questions per board, so that neither the harness
nor a reader has to guess them.

**1. Is this the same design as another fixture?** `dedup` groups fixtures by
exact md5; `variant_family` is a *curated* table (`VARIANT_FAMILIES` in
`manifest.py`) of same-design variants. Adding fixtures means revisiting that
table — it is provenance, not something derived from the bytes.

```
46 files → 45 distinct md5 → 39 independent designs
```

The one byte-identical pair is `Issue110-Pajalnaja_stancija.dsn` /
`Issue110-Паяльная станция.dsn` (the same upstream attachment uploaded twice,
once transliterated). `run_bench.py` counts an md5 group **once** in the
filtered aggregate; the duplicate row is still shown, marked
`byte-identical duplicate of <first member present in the run>`.

**2. What does the board exercise?** Parsed from the DSN with `check_ses.py`'s
tolerant reader: `signal_layers`, `plane_layers` (`(type power)`),
`snap_angle_declared`, `host_cad`/`host_version`, `has_autoroute_settings`,
`has_class_class`, `has_preexisting_wiring` / `has_preexisting_copper` (+ wire
and via counts inside the `(wiring …)` scope), `nets_total` /
`nets_multi_pin`, `components_placed`.
Current suite (the manifest's `coverage` block; reproduce with
`python3 bench/manifest.py`):

| property | value |
|---|---|
| layers | 40 boards 2-layer, 5 boards 4-layer, 1 with no layers at all |
| power planes | 3 boards (Issue145, Issue230, Issue269) |
| `snap_angle` declared | 2 boards, both `fortyfive_degree` — **nothing exercises 90° or none** |
| `host_cad` | 41 KiCad, 2 CadSoft, 3 absent |
| `autoroute_settings` in the DSN | 2 boards |
| `class_class` rules | 0 boards |
| pre-existing wiring | 23 boards have `(wire …)` in the `(wiring …)` scope; 26 have wires **or** pre-placed vias (Issue145 ships 42 vias and no traces) |
| not a Specctra DSN at all | 1 (`Issue006-LPC18XX_43XX_SCH.dsn`, a binary OLE compound document renamed `.dsn`) |

That table is the argument for the mandatory out-of-sample tier in ROADMAP
G2.4: the upstream suite is a 2-layer KiCad monoculture.

**3. What did Java do with it?** `java.class`, read off a real bench run
(`--results`, default `bench/results/20260828-013025/results.json`):

| class | rule |
|---|---|
| `java_timeout` | killed at the run's timeout |
| `java_crash` | signal / unexpected exit code / no parseable completion line |
| `java_degenerate` | finished, but with final score `0.00` **or** 0 multi-pin nets parsed from the DSN — Java never had anything to route (`Issue006`, `Issue229`) |
| `java_finished` | everything else |
| `java_unknown` | fixture absent from the source results (or no manifest loaded) |

On the `-mp 20` / 300 s baseline checked in at `bench/results/20260828-013025`:
29 `java_finished`, 14 `java_timeout`, 1 `java_crash`, 2 `java_degenerate`.
**Regenerate the manifest after the `-mp 9999` re-baseline (ROADMAP M0.5)** or
the classes describe a config nobody runs any more. `manifest.py --check`
exits 1 when the checked-in manifest differs from a fresh generation (CI).

`stability` is `null` on every fixture: it is filled by the 3-run Java sweep
(ROADMAP M0.5). G2.1 treats `unstable` boards like timeout/crash boards, so
null is the conservative state, not a forgotten field.

## Quality filter and the geomean rule

ARCHITECTURE ("Benchmark discipline") and ROADMAP G3.1: the geomean speedup is
computed **only over fixtures that pass the quality predicate in the same
run**. A speedup on a board where we route a third of the nets is not a
speedup. So:

- every row gets a `quality` verdict, and the reason is printed either way
  (`pass`, `java_timeout`, `checker nets 23 < java 74`, …);
- the headline line is labelled **`quality-filtered`** and names the predicate
  and the sample size;
- if nothing passes, the report says `n/a — 0 fixtures passed <predicate>. No
  speed claim may be made from this run.` It never falls back to an unfiltered
  number;
- the unfiltered geomean is printed only with `--emit-unfiltered-geomean`, and
  only tagged `quality-unfiltered, not comparable`. Do not quote it.

Predicates are pluggable (`QUALITY_PREDICATES` in `run_bench.py`, selected
with `--quality-predicate`):

| predicate | meaning |
|---|---|
| `placeholder-nets-v0` (default) | conservative stand-in until fr-check exists: manifest class must be `java_finished`, both tools must finish, and **our** SES must carry routing for at least as many nets as Java's, counted by `check_ses.py` rather than by either tool's self-report |
| `fr-check` | the real G2.1 gate (added violations + unrouted, judged from the SES). Not implemented yet, and deliberately passes **nothing** — a missing referee must never degrade into "everything passes" |

`placeholder-nets-v0` is a floor, not a gate: the coarse checker sees neither
connectivity nor clearance, so "same net count" can still mean "worse board".
It only keeps the geomean off boards where we are visibly routing less.

Current baseline, re-rendered from the checked-in run with
`python3 bench/run_bench.py --render-from bench/results/20260828-013025
--report-out -`:

```
- Quality filter `placeholder-nets-v0`: 5/46 fixtures pass; 1 byte-identical
  duplicate(s) excluded from the filtered aggregate
- Geomean speedup (java/rust), quality-filtered: wall-clock 42.1x (n=5),
  in-tool 16.5x (n=5)
```

Five of forty-six. The unfiltered numbers for the same run are 37.6x / 12.3x
over 29 fixtures — bigger sample, no meaning.

## Metrics

| field | meaning |
|---|---|
| `java_class` | manifest classification of the **Java** reference on this board, copied into every record so a `results.json` is readable on its own. |
| `dedup_group` / `dedup_primary` / `variant_family` | from the manifest; an md5 group is counted once in the filtered aggregate. |
| `config_label` | the `--config` label (e.g. `mp9999-lab`), also in `meta.config_label`. For humans, and for refusing to mix runs. |
| `wall_s` | whole-process wall clock, measured by the harness. For Java this **includes ~9-11s of JVM startup + board init**, which dominates small boards. |
| `tool_time_s` | tool-reported routing time. Java: sum of the "… stage completed: … completed in X seconds" lines (fanout + auto-routing + optimizer). Rust: the "Routing complete in Xs" line. This is the fairer algorithmic comparison. |
| `tool_cpu_s` (Java) | sum of self-reported "total CPU seconds" per stage. |
| `stages` (Java) | per-stage seconds keyed by stage name alone (`fanout`, `auto-routing`, …) — the log line's job-id prefix is not part of the key, so keys aggregate across runs. |
| `time_user_s` / `time_sys_s` | process CPU from `/usr/bin/time -l`. |
| `max_rss_bytes` | peak resident set from `/usr/bin/time -l`. On this macOS the value is in **bytes** (verified empirically: a JVM run reports ~4.2e9). |
| `peak_heap_bytes` (Java) | max self-reported "peak heap usage" across stages (JVM heap only, smaller than RSS). |
| `unrouted` / `violations` | **self-reported, by each tool about itself** — recorded, never gated on; the report marks both columns `(self)`. Java: from the last "final score: S (N unrouted and M violations)" stage line, falling back to the last per-pass line. Rust: derived from "N/M nets routed" and "DRC violations: D". fr-check (ROADMAP M1.4) replaces both. |
| `score` (Java) | Freerouting's own board score (999.99 = clean; 0.00 = nothing to route → `java_degenerate`). |
| `exit_code`, `timed_out`, `crashed` | `crashed` = killed by a signal, unexpected exit code, **or** no parseable completion line. Note: `fr-cli` exits 2 when routing finished with incompletes — that is a result, not a crash. Java can crash internally (stack trace mid-log) and still **exit 0 with a 0-byte SES**, which is why crash detection is parse-based. |
| `ses_bytes` | size of the produced session file. |
| `check` | independent coarse verification from `check_ses.py` (below), totals only. |
| `check_per_net_path` | sidecar file with that run's per-net breakdown. |

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
- `per_net` — the same numbers **per net**: `{pins, wire_count, via_count,
  wire_length_mm, wire_length_raw}` for every net declared in the DSN or
  present in the SES (`pins: null` = present in the SES but never declared).
  Totals cannot express the delta gates that come later: two runs can agree on
  the suite total while disagreeing on every individual net, and a net the
  reference routed but we did not is invisible in a sum.

Standalone use:

```sh
python3 bench/check_ses.py <in.dsn> <out.ses>                # with per_net
python3 bench/check_ses.py --totals-only <in.dsn> <out.ses>  # without
```

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
- Quoted strings do **not** end at the next `"`. KiCad writes inch marks
  unescaped inside them (`(PN "DISPLAY 7-SEG 0.5"")`) and Eagle writes pin
  references as `"J3"-"D+"`. `check_ses.tokenize` therefore ends a string only
  at a `"` followed by whitespace, a paren or EOF. Getting this wrong is
  silent and expensive: the naive scan swallowed 4 of Issue229's 26 `place`
  records and split one Issue143 pin reference into three atoms, inflating
  that board's multi-pin net count from the true 4 to 5.
- Java logs are forced to English with `-l en` (verified working); parsers
  still fall back to the bare `"(N unrouted and M violations)"` fragment and
  accept `,` as decimal separator in case of localization.
- Fixture names may contain spaces and non-ASCII (e.g.
  `Issue110-Паяльная станция.dsn`) — all subprocess calls use argv lists,
  never shell strings, and every JSON write uses `ensure_ascii=False`.
- Timeouts kill the whole process group (`start_new_session` + `killpg`),
  otherwise the JVM survives the death of its `/usr/bin/time` wrapper.
- One "fixture" is not a DSN: `Issue006-LPC18XX_43XX_SCH.dsn` is a binary
  compound document. Java "succeeds" on it in 3.8 s with score 0.00 — exactly
  the kind of free win the `java_degenerate` class exists to keep out of the
  numbers.
