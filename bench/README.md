# bench/ — Freerouting (Java) vs fr-cli (Rust) benchmark harness

This is the project's referee: a head-to-head comparison of the reference
Java Freerouting against our Rust `fr-cli` over the upstream fixture suite.
Every future optimization gets judged by it. Python 3, stdlib only.

| file | role |
|---|---|
| `manifest.py` → `fixtures-manifest.json` | what each fixture *is*: md5 dedup, coverage properties, Java classification, 3-run stability |
| `run_bench.py` | runs both tools, classifies every row from the manifest, writes `results.json` + `report.md` |
| `check_ses.py` | independent (coarse) DSN/SES checker — totals **and per-net** wire length / via count |
| `tools/g21_referee/` | the real referee: `(dsn, ses)` in, one JSON verdict out, same binary for both tools |
| `tools/g21_judge.py` | the ROADMAP G2.1 gate — see "Judging a session" below |
| `test_bench.py`, `tools/tests/`, `analysis/test_campaign.py` | self-tests; hermetic, no fixtures or jar needed |

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

# 4. Judge the run against ROADMAP G2.1 (see "Judging a session")
cargo build --release -p g21-referee
python3 bench/tools/g21_judge.py --ours bench/results/<stamp>/ses

# Harness self-tests (fast, hermetic) and manifest staleness check
python3 bench/test_bench.py
python3 -m unittest discover -s bench/tools/tests
python3 bench/analysis/test_campaign.py
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

**4. Does Java give the same answer twice?** `stability`, folded in from
`bench/results/java-stability/<stem>.r{1,2,3}.log` — three runs of the capped
baseline's exact command line. `stability.class` is what G2.1 reads:

| class | rule | what the gate does with it |
|---|---|---|
| `stable` | ≥2 runs reached a final score and all of them report the same `(unrouted, violations)` | the only class that unlocks the comparative gate |
| `unstable` | ≥2 runs reached one and they disagree (including "sometimes finishes inside the cap, sometimes not") | absolute gate, exactly like a timeout |
| `no_result` | the runs agree there is nothing to report: killed at the cap, or not a routable board | absolute gate |
| `unmeasured` | fewer than two runs on disk — the sweep has not covered this fixture | absolute gate |

`stability.unrouted` / `violations` / `started_unrouted` are **Java's own**
numbers and are comparable only with each other, never with a referee count:
they are what "did the reference make progress on this board?" is answered
with (see the anti-degeneracy guard below).

The logs are gitignored like every other `bench/results/**/*.log`, so
`manifest.py --check` compares on a projection that drops the `stability`
block when they are absent, the same trick it already uses for `uncapped`.

## Judging a session (ROADMAP G2.1)

This is the normative description of the gate. `bench/tools/g21_judge.py`
implements it and `bench/tools/g21_referee/` measures it; changing one without
the others is a bug.

```sh
cargo build --release -p g21-referee
python3 bench/tools/g21_judge.py --ours <dir-of-*.rust.ses> \
    [--java <dir-of-*.java.ses>] [--sweep <our-runner's sweep.json>]
```

### The overlay: which board a session is read into

`read_session` **merges** into the board it is handed, and both tools re-emit
the input board's copper in their session. Reading a session into the full DSN
import therefore inserts every pre-existing trace twice — on `Issue093` that
alone takes the judged violation count from 192 to 322. The session is read
into `fr_io::specctra::strip_session_copper(dsn)` instead: the same DSN with
exactly the copper a session carries removed.

"Exactly" is load-bearing, and emptying the whole `(wiring …)` scope is *not*
it. `SesWriter.writeNet` skips `SYSTEM_FIXED` items, which is what `(type fix)`
copper imports as, so that copper is on the board and in no session file.
`Issue230-CNH_Functional_Tester_1.dsn` is the suite's only board with any (8
wires), and emptying the scope mis-judges it in both directions at once:

| Issue230, same session | unrouted | violations |
|---|---|---|
| our routed board, in process | 5 | 16 |
| judged into an emptied `(wiring)` | 8 | **14** |
| judged into `strip_session_copper` | 5 | 16 |

Three connections lost *and* two clearance violations forgiven. The reference's
own session for that board reads 2/16 through the correct overlay and 8/14
through the emptied one, so the error is not even symmetric between tools.

The **baseline** (`g21-referee <dsn>` with no session) is the *unmodified*
import: `added = judged − baseline` only means anything if the baseline is the
board as delivered.

A violation whose two items are both `USER_FIXED` is never charged to either
tool — it is a property of the input board.

### The two gates

A board is judged **comparatively** only when the reference's answer is a fact
that reproduces: `java.class == java_finished` **and**
`stability.class == stable` **and** a reference session exists.

```
comparative:  added_violations(ours) <= added_violations(java)
         AND  unrouted(ours)         <= unrouted(java)
```

Everything else — timeout, crash, degenerate, unstable, unmeasured, or a
missing reference session — is judged **absolutely**:

```
absolute:  added_violations(ours) == 0
      AND  unrouted(ours) <= unrouted(input)
      AND  (no reference session OR unrouted(ours) <= unrouted(java))
      AND  the anti-degeneracy guard
```

Both gates additionally require `ses_errors == 0`: a session the reader has to
guess at is not a result, however good its numbers look.

### The anti-degeneracy guard

`unrouted <= input` on its own would let "emit an empty session" pass every
absolute board. It cannot, because a board may only stand still when *the
reference implementation could not move it either*. Progress is a recorded
property of the reference — from the manifest, never from the run being
judged — and is looked for in this order:

1. a referee-judged reference session that beats the input;
2. a stability run whose own `unrouted` beat its own `started_unrouted`
   (Java's numbers against Java's numbers);
3. the uncapped campaign: the same self-reported improvement, or the
   `hit_cap`-with-zero-passes case — 2 h in the routing stage without
   finishing a single pass is positive evidence that the board did not move,
   not missing data.

If any of those says the reference made progress, we must too: copper for at
least one multi-pin net **and** `unrouted(ours) < unrouted(input)` strictly. If
they all say it did not, standing still is the correct answer. With no evidence
at all the guard assumes progress is possible and demands it — silence is never
a way through. The guard does not apply to a board that arrives with nothing
unrouted: there is no progress to make, and `< input` would be unsatisfiable.

`Issue229-display-8-digit-hc595.dsn` is why the clause is `<=` and not `<`: 20
of its 64 connections arrive blocked by `USER_FIXED` copper no router may move,
we return the input unchanged with zero added violations, and the reference's
own session for it is **64 unrouted** — 44 connections *worse* than doing
nothing. `Issue208-freerouting.dsn` is why the guard has a "no progress" branch:
the reference spent 7200 s in the routing stage without finishing pass 1 and
all three stability runs were killed at the cap reporting nothing, and we emit a
well-formed, copper-empty session in 66 s that adds no violations and loses no
connection.

### The fidelity check

With `--sweep`, the judge also compares the router's own in-process referee
line against the same referee reading the session back. A gap means the writer,
the reader or the overlay lost or invented something — it is what found the
`(type fix)` overlay bug above. It is reported per run and is expected to be
empty.

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
| `exit_code`, `timed_out`, `crashed` | `crashed` = killed by a signal, unexpected exit code, **or** no parseable completion line. Note: `fr-cli` exits 2 when routing finished with incompletes and 4 when its `--job-timeout` budget ran out — both are results, not crashes. The harness passes `--job-timeout (--timeout minus 10 s)` on every rust run so the tool stops itself, restores its best board and writes a session, instead of being killed at the cap with nothing on disk. Java can crash internally (stack trace mid-log) and still **exit 0 with a 0-byte SES**, which is why crash detection is parse-based. |
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
