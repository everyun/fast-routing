# bench/tools/ — measurement contracts and the differential harness

Two instrumentation streams feed the benchmark and gate machinery. This file is
their normative contract; anything that emits or parses them must match it.

| stream | producer | consumer |
|---|---|---|
| `FRLAB …` | the instrumented **lab jar**, `/tmp/fr-lab-release/freerouting-lab.jar` | `frlab_parse.py` |
| `FRMETRIC …` | our router, via the **`fr-metrics`** crate | `frlab_parse.py`, `run_bench.py` |

`frlab_parse.py` parses both and diffs the settle sequences against each other.
That diff is the M2 oracle: it answers *"did our maze engine visit the same
frontier elements, in the same order, as the reference?"* — a question no score
comparison can answer, and the one that catches search drift while it is still a
one-line bug (`docs/ROADMAP.md`, standing risk *"shove/maze drift invisible until
late"*).

Stdlib Python 3 only, like the rest of `bench/`.

## Quick start

```sh
# 1. Java reference trace (fanout+route; nice, the machine is usually busy)
nice -n 15 java -Dfrlab.settle_trace=1 -jar /tmp/fr-lab-release/freerouting-lab.jar \
    -de /tmp/freerouting-upstream/fixtures/Issue026-J2_reference.dsn \
    -do /tmp/java.ses -mp 3 -l en > /tmp/java.log 2>&1

# 2. Our trace — from M2a on, once the maze engine calls
#    fr_metrics::settle_trace::settle(). FR_SETTLE_TRACE takes `1`, `stderr`
#    or a path. The contract below is fixed now so both sides can be built
#    against it; today only the Java side produces data.
FR_SETTLE_TRACE=/tmp/ours.log target/release/fr-cli \
    -de /tmp/freerouting-upstream/fixtures/Issue026-J2_reference.dsn \
    -do /tmp/ours.ses -mp 3

# 3. Per-pass totals, and the diff
python3 bench/tools/frlab_parse.py summary /tmp/java.log
python3 bench/tools/frlab_parse.py diff /tmp/ours.log /tmp/java.log --window 5

python3 bench/tools/frlab_parse.py --self-test    # 16 tests, no fixtures needed
```

`diff` exits **0** when the sequences agree and **1** when they diverge, so it
drops straight into a gate script. `--json` on either subcommand emits one JSON
object per log / one for the diff.

---

## The `FRLAB` contract (Java lab jar)

Built from the **release revision** `2d4de019` — the same revision as the shipped
2.3.0 jar — and proven non-perturbing: identical per-pass board hashes and
identical SES against the stock jar (`docs/ROADMAP.md` M0.5). Two system
properties switch its behaviour:

| property | effect |
|---|---|
| `-Dfrlab.settle_trace=1` | emit one `FRLAB settle` line per expansion |
| `-Dfrlab.retain_autoroute_database=true` | retain the per-connection autoroute DB — **changes routing behaviour** (3.2× expansions, converges worse), so calibration runs must leave it off |

Three record types, emitted through `FRLogger.info`, so each is preceded by the
logger's timestamp and level and **never starts the line**:

```
2026-08-28 06:20:25.287 INFO   FRLAB expansions_pass=7691 pass=1
2026-08-28 06:20:25.288 INFO   FRLAB expansions_conn n=45 p50=11 p99=1489 max=1489 pass=1
2026-08-28 06:20:24.598 INFO   FRLAB settle 1086 0 0.0 231507.24242124692
```

### `FRLAB expansions_pass=<N> pass=<P>`

`N` = maze elements settled during pass `P`. Emitted by
`BatchAutorouter.autoroute_pass` at the **end** of the pass (also on the
early-return path where the item list is empty, so every pass has exactly one
line). The counter is a global `AtomicLong` snapshotted at pass start, so `N` is
a difference, not a running total.

### `FRLAB expansions_conn n=<N> p50= p99= max= pass=<P>`

Per-connection expansion distribution for pass `P`, drained (and cleared) at the
same point. `N` is the number of connections routed in the pass. Percentiles are
computed by upstream's own `frlab_drain_connection_histogram`, over a sorted
`List<Long>`, with `p50 = floor(0.5·(n−1) + 0.5)` and `p99 = ceil(0.99·(n−1))` —
nearest-rank, not interpolated. Do not re-derive them differently when comparing.

perf-forensics §8.6 is why this record exists: *"watch the tail, not the mean …
a pathological connection is what kills the suite."*

### `FRLAB settle <door_id> <section_no> <g> <f>`

One line per element popped off the frontier and expanded, emitted from
`MazeSearchAlgo.occupy_next_element` **before** the backtrack fields are written
and before the element is marked occupied. Fields:

| field | source | notes |
|---|---|---|
| `door_id` | `list_element.door.get_id_no()` | **signed 32-bit and frequently negative** — see below |
| `section_no` | `list_element.section_no_of_door` | small non-negative int (0–9 on Issue026) |
| `g` | `list_element.expansion_value` | Java `double`, printed by `Double.toString` |
| `f` | `list_element.sorting_value` | Java `double`, the frontier's primary sort key |

`(door_id, section_no)` is the **topological identity** of the settle and is what
the gate compares. `g`/`f` are f64 and are compared with a tolerance —
per `docs/ARCHITECTURE.md`, f64 never decides topology, so a g/f drift with an
intact order is a weaker (though still reportable) finding than an order swap.

> **Porting constraint discovered here:** on `Issue026-J2_reference`, 1853 of
> 8718 door ids are negative (range `-2139373184 … 2136325847`, all inside
> `i32`). Upstream's `getId()` formulas (`ExpansionDoor.java:185-190`,
> `ExpansionDrill.java:127-130`, `DrillPage.java:190-193`, `Simplex.java:70-77`,
> `IntPoint.java:126-128`, `Line.java:53-55`) overflow a Java `int` and wrap. The
> Rust port must use **wrapping `i32`** arithmetic for them, not `i64`/`u32`:
> these ids are the tie-breaks in `MazeListElement`'s four-level ordering
> (maze-engine.md §9.1), so getting the wrap wrong silently reorders the
> frontier.

Java prints doubles as `0.0`, `1.0E7`, `Infinity`, `-Infinity` or `NaN`; the
parser accepts all of them (Python's `float()` does too).

---

## The `FRMETRIC` contract (`fr-metrics`)

Produced by `crates/fr-metrics`, whose design is `docs/upstream-study/perf-forensics.md`
§8 turned into code. The `metrics` cargo feature is on by default; with
`--no-default-features` every entry point is an empty `const fn` and the calls
disappear (proved in `crates/fr-metrics/tests/zero_overhead.rs`, which builds the
disabled variant and asserts the assembly of an instrumented loop contains no
call instruction).

### Report lines — `FRMETRIC <json>`

`MetricsReport::to_frmetric_lines()` emits the literal prefix `FRMETRIC ` and
then **exactly one JSON object on exactly one line**, so `grep FRMETRIC` +
`json.loads` is a complete parser. Four `kind`s:

```
FRMETRIC {"kind":"counters","expansions":8718,"queue_pushes":0,…,"pull_tight_iterations":0}
FRMETRIC {"kind":"stage","pass":1,"stage":"route","nanos":1020000000,"calls":1}
FRMETRIC {"kind":"pass","pass":1,"unrouted":2,"duration_ms":1020,"ripup_cost":100}
FRMETRIC {"kind":"alloc","total_bytes":…,"peak_bytes":…,"live_bytes":…,"allocations":…,"deallocations":…}
```

- **`counters`** — one `u64` per counter, always all of them, in a fixed order.
  `expansions` is the field to compare against `FRLAB expansions_pass` (summed
  over passes). `door_shape_computations` and the per-expansion allocation
  counts are the ones expected to be **zero** in steady state: perf-forensics
  §8.1 — *"if a benchmark shows non-zero, that is a bug, not a tuning
  opportunity."*
- **`stage`** — one row per `(pass, stage)`, sorted by pass then stage. The
  stage names `item_selection`, `board_statistics`, `intermediate_statistics`,
  `autoroute_item`, `maze_search`, `opt_changed_area`, `tail_removal`,
  `incomplete_drc` mirror upstream's eight `profile*Nanos` accumulators
  one-for-one (perf-forensics §1.3), so a run of ours is directly comparable to
  a Java run under `-Dfreerouting.benchmark.profile=true`. `fanout` / `route` /
  `optimize` are the batch stages `run_bench.py` already parses out of Java's
  log. Pass `0` is the bucket for work outside any router pass.
- **`pass`** — upstream's pass history tuple (perf-forensics §8.4), the cheapest
  "is my search behaving like the reference?" signal.
- **`alloc`** — only present in an `--features fr-metrics/alloc-stats` build,
  which installs a counting global allocator. `total_bytes` is the analogue of
  the JVM's `getThreadAllocatedBytes` (the number the 3 TB figure comes from),
  except process-wide and exact.

Every key and every string value is a `[a-z_]+` identifier and every other value
is an unsigned integer, so the JSON needs no escaping and none is applied.

### Settle lines — `FRMETRIC settle <door_id> <section_no> <g> <f>`

The one non-JSON `FRMETRIC` record, deliberately identical in shape to
`FRLAB settle` so a single parser and a single diff serve both sides. Off unless
`FR_SETTLE_TRACE` is set:

| `FR_SETTLE_TRACE` | sink |
|---|---|
| unset or `0` | disabled |
| `1` or `stderr` | standard error |
| anything else | that path, truncated on open |

It is a diagnostic mode — one formatted, buffered line per expansion. Never
benchmark with it on, and always let the process exit cleanly (or call
`fr_metrics::settle_trace::flush()`) so the tail of the buffer reaches disk.

---

## `frlab_parse.py`

### As a library

```python
import sys; sys.path.insert(0, "bench/tools")
import frlab_parse as fp

log = fp.parse_log("/tmp/java.log")          # or fp.parse_lines(iterable)
log.total_expansions                         # 8718
log.passes[0].expansions, log.passes[0].conn_p99
log.settles[0].key                           # (1086, 0)
log.settles_of_pass(1)

result = fp.diff_settles(ours.settles, java.settles,
                         window=5, tolerance=1e-9,
                         left_name="ours", right_name="java")
result.identical, result.kind, result.first_divergence
print(result.render())
```

`parse_log(path, max_settles=N)` caps the settle list (large boards trace
millions of lines) while still parsing every pass record and setting
`log.truncated`. `iter_settles(path)` streams settles for logs that do not fit in
memory.

Settles are attributed to the pass whose `expansions_pass` line **follows** them,
because upstream logs that line at the end of the pass. `PassRecord.consistent`
cross-checks the settle lines seen against the pass's own total; a `MISMATCH` in
the summary table means the trace is truncated, or `--limit` was used, or
expansions happened outside a pass window (the fanout stage runs maze searches
too) — investigate before trusting a diff.

### Divergence kinds

`diff_settles` reports the **most severe** applicable kind, in this order:

| kind | meaning | M2 gate |
|---|---|---|
| `topology` | different `(door_id, section)` at `first_divergence` — the routers took different paths from there | **fail**: this is search drift |
| `length` | keys agree over the whole shorter run, then one run stops | investigate: a stop rule, time limit or truncated trace |
| `value` | same order, `g`/`f` beyond tolerance at `first_divergence` | report: numeric precision, not routing behaviour |
| `identical` | same keys, `g`/`f` within tolerance | pass |

`render()` prints a context window of `--window` records either side of the
divergence, from both runs, with `>` on the diverging row:

```
kind=topology matched_prefix=4999 first_divergence=4999
ours settled (3986, 0), java settled (3985, 0)

     idx     ours                                    java
    4998     door=404849 sec=0 g=179829 f=407066     door=404849 sec=0 g=179829 f=407066
    4999  >  door=3986 sec=0 g=163000 f=407093       door=3985 sec=0 g=163000 f=407093
    5000     door=404818 sec=0 g=180813 f=408499     door=404818 sec=0 g=180813 f=408499
```

---

## How M2 uses this

`docs/ROADMAP.md` M2a: *"Oracle: settle-order agreement with the lab jar on 2
instrumented boards."* Per slice:

1. **M2a (maze + locate + insert + fanout).** Generate the Java trace once per
   instrumented board and keep it as a golden file. After every change to the
   maze engine, run ours and `diff`. The gate is `kind == identical`; while the
   engine is still being built, the *useful* metric is `matched_prefix` — it
   should grow monotonically, and a drop is a regression even when both runs
   still fail. Deterministic tie-breaks must be enabled on our side
   (`--deterministic`), because the whole comparison rests on
   `MazeListElement`'s four-level ordering (maze-engine.md §9.1).
2. **M2b (shove + ripup + pull-tight).** Same, on the shove-heavy set. A shove
   that succeeds where Java's failed shows up as a `topology` divergence at the
   first element after the shove, which is exactly the localisation a score
   comparison cannot give.
3. **M2c (batch loop + optimizer).** Compare `FRMETRIC counters.expansions`
   summed per pass against `FRLAB expansions_pass`, and the per-connection
   distribution against `FRLAB expansions_conn`, before comparing settle order —
   a large expansion-count delta with an intact settle prefix means the *stop
   rules* diverged, not the search.

The `FRMETRIC stage` rows carry the other half of M3's budget: they are the only
numbers directly comparable to Java's `-Dfreerouting.benchmark.profile=true`
split, which is what M0.5's measurement campaign is producing.

---

## Recorded reference numbers

`Issue026-J2_reference.dsn`, lab jar, `-mp 3 -l en`, settle trace on,
`nice -n 15`, 2026-08-28:

| pass | `expansions_pass` | `n` | `p50` | `p99` | `max` |
|---|---|---|---|---|---|
| 1 | 7691 | 45 | 11 | 1489 | 1489 |
| 2 | 876 | 10 | 19 | 432 | 432 |
| 3 | 151 | 4 | 30 | 93 | 93 |

Total 8718 expansions, 8718 settle lines, every pass `consistent` — so on this
board the fanout stage contributes no settles and the settle stream partitions
exactly into passes. Final: 2 unrouted, 0 violations, 1.02 s auto-routing stage.

**The oracle is deterministic:** two independent runs of the lab jar on this
board produced settle sequences that `diff` reports as `identical` over all 8718
records. Re-check this before trusting a divergence on any newly instrumented
board — a non-deterministic reference would make the gate meaningless.

## Caveats

- The settle trace costs real time and log volume (~8.7 k lines for a board that
  routes in one second; millions on the big ones). It is an oracle, not a
  benchmark configuration.
- `p50`/`p99` come from upstream's own nearest-rank code. If you compute
  percentiles on our side, reproduce that formula or the comparison is invalid.
- `retain_autoroute_database=true` is **not** a free performance lever; it
  changes results. Every calibration and every differential run uses fresh-DB
  mode.
- The lab jar's log prefix (timestamp, level, job id) is not part of the
  contract — the parser anchors on the `FRLAB`/`FRMETRIC` token anywhere in the
  line, and tolerates any prefix or none.
