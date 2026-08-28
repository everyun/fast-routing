# bench/tools/ — measurement contracts and the differential harness

Three data contracts feed the benchmark and gate machinery. This file is their
normative definition; anything that emits or parses them must match it.

| contract | producer | consumer | gate |
|---|---|---|---|
| `FRLAB …` | the instrumented **lab jar**, `/tmp/fr-lab-release/freerouting-lab.jar` | `frlab_parse.py` | M2a–c settle order |
| `FRMETRIC …` | our router, via the **`fr-metrics`** crate | `frlab_parse.py`, `run_bench.py` | M2/M3 counters |
| **golden-dump JSON** (`fast-routing.golden-dump/1`) | `fr_io::golden_dump` (CLI: the `golden_emit` example) and the Java-dump adapter | `golden_compare.py` | **G1.0 / G1.1 import parity** |

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

# 3. Per-pass totals, the aggregate comparison, and the positional diff
python3 bench/tools/frlab_parse.py summary /tmp/java.log
python3 bench/tools/frlab_parse.py stats  /tmp/ours.log /tmp/java.log
python3 bench/tools/frlab_parse.py diff   /tmp/ours.log /tmp/java.log --window 5

# …and, once a door-id mapping exists, the opt-in topology comparison:
python3 bench/tools/frlab_parse.py diff /tmp/ours.log /tmp/java.log --door-map doors.json

python3 bench/tools/frlab_parse.py --self-test    # 31 tests, no fixtures needed
```

`diff` exits **0** when the sequences agree and **1** when they diverge, so it
drops straight into a gate script. `--json` on any subcommand emits one JSON
object per log / one for the diff or the stats.

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

`(door_id, section_no)` is the **topological identity** of the settle *within one
run*. It is **not comparable across runs**, and the diff does not compare it
unless you supply a mapping — see [Why the diff is positional](#why-the-diff-is-positional).
`g`/`f` are f64 and are compared with a stated tolerance; per
`docs/ARCHITECTURE.md`, f64 never decides topology.

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

### Why the diff is positional

A door id is a dense counter handed out in door-creation order, and door
creation order follows the room tiling. Ours is our counter, upstream's is
theirs. **An id-keyed diff therefore answers `kind=topology
first_divergence=0` on every pair of logs**, bit-identical searches included —
which is exactly what M2a hit, and it carries no information.

So `diff_settles` compares **settle *k* of one run against settle *k* of the
other, on the cost pair `(g, f)`**, under a tolerance that the report always
states. Costs are in the same board units on both sides, and they are the
quantity the frontier order is derived from, so they *are* comparable.

`(door_id, section)` is compared only when you opt in with `--door-map FILE`,
which translates our ids into the reference's space first. The file is either a
JSON object (`{"doors": {"97": 1086}}`, or the bare `{"97": 1086}`) or
`<ours> <reference>` lines with `#` comments. Ids missing from the map are left
untranslated and counted in a warning, so a partial map still diffs what it
covers.

### Divergence kinds

`diff_settles` reports the **most severe** applicable kind, in this order:

| kind | meaning | M2 gate |
|---|---|---|
| `topology` | **`--door-map` only.** Different `(door_id, section)` at `first_divergence` | **fail**: search drift, given a trusted mapping |
| `value` | `g` or `f` beyond tolerance at position `first_divergence` — from there the two frontiers hold different costs | **fail**: the orders cannot be expected to agree past this point |
| `length` | `(g, f)` agrees over the whole shorter run, then one run stops | investigate: a stop rule, time limit or truncated trace |
| `identical` | same length, `(g, f)` within tolerance at every position | pass |

### Aggregates

Every `diff` also prints — and `stats <left> <right>` prints on its own — the
metrics that survive a renumbering: settles, `expansions_pass` per pass,
distinct doors, distinct `(door, section)` keys, and the `g`/`f` distributions
(`min/p50/p90/p99/max/mean`, nearest-rank, non-finite values counted separately
rather than folded into the tail). The `ratio` column is left/right.

These are what actually separate *"we search more"* from *"we search the same
thing more slowly"*. On `Issue034-Green14SegLED` at `-mp 1`:

```
metric                      ours      lab jar    ratio
settles                    88412        41680    2.121
distinct doors              8242        17721    0.465
distinct (door,sec)        15365        24867    0.618
  g p50                   267604       214603    1.247
  f p50                   468818       429633    1.091
  f min                    23273        23273    1.000
```

Identical `f min`, near-identical distributions, but 2.1× the settles and *half*
the distinct doors: the cost function agrees and the tiling is coarser, so the
extra work is re-expansion, not a different cost model. Pass `--no-aggregates`
to suppress the block.

`render()` also prints a context window of `--window` records either side of the
divergence, from both runs, with `>` on the diverging row:

```
compared: g/f by sequence position, tolerance=1e-09 (rel and abs)
kind=value matched_prefix=4999 first_divergence=4999
g/f drift at position 4999: ours g=163000.0 f=407093.0, java g=163000.0 f=407094.5

     idx     ours                                    java
    4998     door=404849 sec=0 g=179829 f=407066     door=404849 sec=0 g=179829 f=407066
    4999  >  door=3986 sec=0 g=163000 f=407093       door=3985 sec=0 g=163000 f=407094
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

---

# `golden_compare.py` — the G1.0/G1.1 golden-dump comparator

`java -de f.dsn -do f.import.dsn --router.enabled=false
--router.fanout.enabled=false --router.optimizer.enabled=false` makes the
release jar import a board and immediately serialise it back out through
`DsnWriter`. `bench/golden/dumps/<fixture>.import.dsn` is therefore **not** the
input file — it is Java's *resolved board model*: clearance classes
materialised, placements resolved, net classes bound, pre-existing wiring
re-imported with its fixed states. That is the G1.0 oracle, and `ROADMAP` G1.1
gates our importer against it.

`golden_compare.py` is pure stdlib Python with **no dependency on the Rust
tree**, so the harness lands and is validated before the importer exists. It
reuses `bench/check_ses.py`'s tolerant S-expression machinery rather than
carrying a second lexer.

```sh
# One board, field-level diff report; exit 0 = agree, 1 = differ, 2 = usage/IO
python3 bench/tools/golden_compare.py \
    --java-dump bench/golden/dumps/Issue026-J2_reference.import.dsn \
    --ours /tmp/ours.json

# Every fixture, summary table (self-comparison when --ours-dir is omitted)
python3 bench/tools/golden_compare.py --all [--ours-dir target/golden]

# The reference encoder: adapt a Java dump into the exchange schema
python3 bench/tools/golden_compare.py --emit <dump> > /tmp/java.json

python3 bench/tools/golden_compare.py --exclusions   # the lossy axes, verbatim
python3 bench/tools/golden_compare.py --self-test    # 99 tests
```

Useful flags: `--gate-only` (compare only the G1.1 groups), `--float-decimals N`
(quantisation, default 6), `--max-diffs N` / `-v`, `--json`.

## The exchange format — `fast-routing.golden-dump/1`

Both sides produce this document; the comparator only ever sees the document,
never a board. The Rust side emits it from **`fr_io::golden_dump::emit`** — the
CLI is `cargo run -p fr-io --release --example golden_emit -- in.dsn out.json`,
and `crates/fr-io/tests/golden_gate_test.rs` calls the same function in-process
for a five-board subset. The Java side gets it from the adapter built into this
file, which is what makes the Java-vs-Java self-comparison a real test of the
parser *and* the comparator.

Everything is JSON. Numbers are JSON numbers (never strings). `null` means "the
model has no value here", which is distinct from `0`.

```jsonc
{
  "schema":   "fast-routing.golden-dump/1",   // required, exact
  "producer": "fr-cli 0.1.0",                 // metadata, not compared
  "source":   "Issue026-J2_reference.dsn",    // metadata, not compared
  "design":   "Issue026-J2_reference",        // metadata, not compared

  "unit":       "um",                          // lowercase
  "resolution": { "unit": "um", "value": 10 },

  // ORDER IS SEMANTIC — index decides layer flip and matrix indexing.
  "layers": [ { "name": "F.Cu", "type": "signal" },     // "signal" | "power"
              { "name": "B.Cu", "type": "signal" } ],

  "rules": {
    // (class-pair, layer) -> value, already expanded per layer: a layer with
    // no rule of its own repeats layer 0's row. `classes` is a two-element
    // array, so the DSN spelling ambiguity (smd / smd_smd / smd-smd) is gone.
    "clearance_matrix": [
      { "classes": ["default", "default"], "layer": "F.Cu", "value": 200.2 },
      { "classes": ["smd", "smd"],         "layer": "F.Cu", "value": 50.0  }
    ],
    "default_trace_width": { "F.Cu": 250.0, "B.Cu": 250.0 },  // per layer
    "smd_to_turn_gap":     { "F.Cu": 125.0, "B.Cu": 125.0 },  // per layer
    "snap_angle":  "fortyfive_degree",        // "none" | "fortyfive_degree" | "ninety_degree"
    "via_at_smd":  "off"                      // "on" | "off" | null
  },

  "padstacks": [ {
    "name": "Round[A]Pad_1358_um",
    "attach":   false,          // attach_allowed; DSN omits `(attach off)`'s inverse
    "absolute": false,          // placed_absolute
    "shapes": [ { "kind": "circle", "layer": "F.Cu", "coords": [1358.0, 0.0, 0.0] } ]
    // kind: "circle" (d[,x,y]) | "rect" (x1 y1 x2 y2) | "polygon" (aperture x y …)
    //     | "path"/"polyline_path" (width x y …). `coords` is every number after
    //     the layer name, in file order.
  } ],

  "images": [ {
    "name": "TE_1888019-6:TE_1888019-6",
    "side": "front",                                   // "front" | "back"
    "pins": [ { "padstack": "…", "name": "S1", "x": -8900.0, "y": -5720.0, "rotation": 0 } ],
    "keepouts": 4, "via_keepouts": 0, "place_keepouts": 0,   // counts
    "outlines": [ /* area objects, see below */ ]
  } ],

  "components": [ {
    "name": "J2", "image": "TE_1888019-6:TE_1888019-6",
    "placed": true, "x": 135255.0, "y": -85725.0,
    "side": "front", "rotation": 90, "position_fixed": false,
    "pins":     [ { "name": "S1", "clearance_class": "default" } ],
    "keepouts": [ { "kind": "keepout", "name": "keepout_1", "clearance_class": "default" } ]
    // kind: "keepout" | "via_keepout" | "place_keepout"
  } ],

  "nets":        [ { "name": "GND", "subnet": 1, "pins": [ ["U1", "10"], ["J2", "B18"] ] } ],
  "net_classes": [ {
    "name": "default", "nets": ["GND", "+3V3"],
    "clearance_class": "default", "via_rule": "default", "trace_width": 250.0,
    "use_layer": ["F.Cu", "B.Cu"], "length_max": null, "length_min": null,
    "pull_tight": true, "shove_fixed": false
  } ],

  "via_padstacks": ["Via[0-1]_800:400_um", "Via[0-1]_800:400_um"],  // structure (via …), multiplicity kept
  "via_infos":     [ { "name": "…", "padstack": "…", "clearance_class": "default", "attach": false } ],
  "via_rules":     [ { "name": "default", "vias": ["Via[0-1]_800:400_um"] } ],

  "wiring": {
    "by_kind_fixed": [ { "kind": "trace", "fixed": "user_fixed", "count": 1462 } ]
    // kind:  "trace" | "via" | "conduction_area"
    // fixed: "unfixed" | "shove_fixed" | "system_fixed" | "user_fixed"
  },

  "boundary":        [ /* area objects: bounding box first, then the outline */ ],
  "structure_areas": [ /* area objects; planes also carry "net" */ ]
}
```

**Area object** (board outline, structure keepouts, planes, image outlines) —
deliberately reduced, see the exclusions:

```jsonc
{ "kind": "keepout",        // boundary | outline | keepout | via_keepout | place_keepout | plane
  "shape": "polygon",       // the geometric primitive, or null
  "layer": "F.Cu",
  "corners": 37,
  "bbox": [xmin, ymin, xmax, ymax],   // or null
  "windows": 0,                        // hole count
  "clearance_class": null,
  "net": "GND" }                       // planes only
```

### Fixed-state spelling

The DSN dump writes `Wiring.write_fixed_state`'s four cases; the schema uses the
board-model names, because `fix` vs `protect` is not self-explanatory:

| DSN `(type …)` | schema `fixed` | Java `FixedState` |
|---|---|---|
| *absent* | `unfixed` | `UNFIXED` |
| `shove_fixed` | `shove_fixed` | `SHOVE_FIXED` |
| `fix` | `system_fixed` | `SYSTEM_FIXED` |
| `protect` | `user_fixed` | `USER_FIXED` |

## Field groups and what G1.1 gates

| group | gate | shape | comparison |
|---|---|---|---|
| `resolution` | G1.1 | map | exact (unit lowercased) |
| `layers` | G1.1 | **ordered list** | index by index |
| `clearance_matrix` | G1.1 | map `(a,b)@layer` | value, diagonal only |
| `padstacks` | G1.1 | map by name → per-layer shapes | exact coords |
| `components` | G1.1 | map by name, recursed to `C.pin.<name>` | placement + per-pin clearance class |
| `nets` | G1.1 | map `name#subnet` → pin bag | membership |
| `wiring` | G1.1 | map `kind/fixed` | counts |
| `via_rules` | G1.1 | multiset (via infos + via rules + structure padstack list) | with multiplicity |
| `images` | extra | map by name, recursed to `I.pin.<name>` | pin → padstack binding |
| `net_classes` | extra | map by name | class → clearance/via rule/width/layers |
| `rules_misc` | extra | map | snap angle, via_at_smd, per-layer width and smd gap |
| `boundary` | extra | multiset of areas | reduced geometry |
| `structure_areas` | extra | multiset of areas | reduced geometry |

`extra` groups are compared by default (they are free, and they localise a
failure faster than a clearance-matrix delta does) but are not part of the G1.1
gate; `--gate-only` drops them.

Differences are reported as `-` (only on the left), `+` (only on the right),
`~` (both present, values differ), and are recursed into nested objects so the
report names the field:

```
components (2 difference(s))
  ~ J1.pin.D+: left="smd"  right="via"
  ~ J1.rotation: left=90.0  right=180.0
nets (1 difference(s))
  + D+#1.pin.Xnew-1: left=<absent>  right=1
```

## Known-lossy exclusions

`--exclusions` prints these; the authoritative list is `EXCLUSIONS` in the
source. Summarised:

1. **Class-pair spelling.** `Rule.write_named_clearance_rules` emits a diagonal
   cell as `(type smd)`, while the reader also accepts `smd_smd`, `smd-smd` and
   `"smd"_smd` (dsn-board-semantics.md §7.3). All normalise to the unordered
   pair `("smd","smd")`. The disambiguation is *quoting*: `IdentifierType`
   treats `_` and `-` as reserved, so `(type "board_edge")` is one class while
   bare `(type a_b)` is a pair. The JSON schema removes the ambiguity entirely
   by carrying an explicit two-element `classes` array.
2. **Clearance-matrix off-diagonal.** `write_non_default_clearance_rules` is
   commented out in the release revision (`parser/Rule.java:185`), so the dump
   carries only the diagonal plus `(default,default)`. Off-diagonal cells are
   projected away on both sides and counted in the report. Off-diagonal parity
   is gated by **G1.2a** (`java -drc`), not here — do not "fix" G1.1 by trusting
   an oracle that cannot see the field.
3. **Float formatting.** Java prints doubles with `String.valueOf`. Every number
   is quantised to `--float-decimals` (6) decimals and `-0.0` folds to `0.0`.
   A pair that survives quantisation but lands within one quantum is reported
   as a `float-noise` note, not a difference — a gate must not depend on which
   side of a rounding boundary a decimal fell.
4. **Ordering.** Collections whose DSN order is Java-internal iteration order
   (`UndoableObjects` traversal, library insertion order) are compared as
   multisets or name-keyed maps. Order is kept only where it is semantic: the
   layer list.
5. **Design name and provenance.** `(pcb <name>)` is the `-do` file stem; the
   `(parser …)` scope is host metadata. Recorded, never compared.
6. **Area geometry reduction.** Board outline, structure keepouts, planes and
   image outlines are compared as `(kind, layer, net, clearance class, corner
   count, bounding box)`. Padstack shapes — small, and what clearance is
   computed against — are compared corner-exact.
7. **Component rotation.** `Component.write_scope` writes the placement angle as
   `(int) Math.round(getRotationInDegree())` (`parser/Component.java:67`) — an
   *integer* degree, while the board model holds a `double`. A footprint at
   352.5° is written `353`. Both sides go through the same half-up rounding
   before comparison, so sub-degree placement parity is not observable here
   (`Issue157-TeamAdapt-LinePCB` has 36 such placements, `Issue214-freerouting`
   12).
8. **Conduction-area fixed state.** `Wiring.write_conduction_area_scope`
   (`parser/Wiring.java:148-180`) never calls `write_fixed_state`, so every
   conduction area reads back `unfixed` even though `Structure.java:967` inserts
   planes `SYSTEM_FIXED`. The `wiring` group folds `conduction_area` into one
   `unfixed` bucket on both sides. The *scope* split is not lossy and is **not**
   folded: `Wiring.write_scope` writes only the conduction areas on **signal**
   layers and `Structure.write_conduction_areas` writes the power-layer ones as
   `(plane …)`, so an emitter must split them the same way or it shows a phantom
   `conduction_area` and a missing `plane`.
9. **Pre-normalisation trace splitting.** Wiring is counted by
   `(kind, fixed state)`, not matched record by record: upstream re-imports
   traces with `insert_trace_without_cleaning` and normalises afterwards
   (§10.4), so the split of a net's copper into `(wire …)` records is an
   artefact of insertion order.

Two further losses are **not** normalised away, because they are model
differences the report should keep showing:

- **Empty identifiers vanish.** `IdentifierType.write("")` emits nothing at all,
  so a net class named `""` (`Issue179` has a literal `(class  (circuit …))`)
  writes as `(via_rule\n   "via0-"\n)` — the rule name disappears and the first
  via name slides into its place. The dump cannot represent the model here.
- **Non-ANSI characters in unquoted identifiers.** The shipped scanner drops
  every code point above `U+00FF` from an unquoted token, with a
  `Non-ansi character '…' found at position …` warning
  (`SpecctraDsnStreamReader.java:1277`, action `case 2`), so
  `Issue110-RelayModule`'s `(image Предохранители:Цилиндр_5х20` becomes `:`.
  Quoted strings are unaffected — the same file's `"Соединители:KF301-2P"`
  round-trips. Our importer keeps the name; see the "Deliberate divergences"
  section of `crates/fr-io/src/specctra/mod.rs`.

## When the oracle itself failed

A dump whose Java run did not finish reading the fixture is not a resolved board
model, and diffing against it measures the failure. `--all --ours-dir` therefore
reads the sibling `<fixture>.log` and marks the row **`ORACLE-FAILED`** — not
compared, not counted in the denominator, listed under the table with the log
line that proves it. The markers are in `ORACLE_READ_FAILURE_MARKERS`; nothing is
hard-coded per fixture, so a re-run of the jar that succeeds silently re-enables
the board.

| fixture | marker | what happened |
|---|---|---|
| `Issue006-LPC18XX_43XX_SCH` | `parse error while reading board file` | the file is an OLE2 compound document, not DSN (the manifest files it under `coverage/not_specctra_dsn`); the dump beside that log does not describe it |
| `Issue229-display-8-digit-hc595` | `read_place_scope` | a `'` inside `(PN …)` opens a string the scanner never closes, so the placement scope dies mid-scan and `placement`, `library`, `network` and `wiring` all come out empty |

## Why the dump text is canonicalised before parsing

`check_ses.tokenize` ends a quoted atom at the first `"` followed by whitespace,
a paren or EOF — right for KiCad *input* files, which contain unescaped inch
marks. It is wrong for a Java-written dump: `-` is a reserved character, so
`Net.write_pin` emits pin references as `"COMP"-"PIN"`, a closing quote followed
by `-`, and the tolerant scan runs past it and swallows the rest of the file.
Five of the 43 dumps contain such references (`Issue219-LogicBoard_smt` has 166).

Rather than fork the tokenizer, `canonicalise_dump_text` runs first. Because
`IdentifierType.write` strips the string-quote character out of every name, in a
dump quotes strictly alternate, so each quoted region can be rewritten as a
marker-prefixed bare atom with its delimiter-significant characters escaped.
After that the text contains no `"` at all, the tolerant path is exact, and the
marker is what later tells a quoted class name from a class *pair*. If
`check_ses` ever gains a strict mode, delete the pre-pass — do not keep two
lexers. `test_tolerant_tokenizer_alone_mangles_the_same_input` pins the reason.

## Validation status

2026-08-28, `bench/golden/dumps/` (43 dumps, 46 fixtures in the manifest):

* `--all` self-comparison: **43/43 clean**, 0 differences, ~1.7 s.
* `--self-test`: 99 tests, including a shuffle/jitter/spelling property triple
  (the comparator must be blind to the three lossy axes) and its converse
  (a real change in each compared field must be caught).
* Cross-checked against an independent, section-scoped line counter: components,
  padstacks, images, nets, net classes, via infos, via rules, wiring wires,
  wiring vias, structure areas, boundaries, placement pins and net pin
  references all agree on all 43 boards — the sweep is not passing by returning
  empty documents.
* Aggregate extracted: 96 layers, 616 clearance cells, 733 padstacks, 586
  images, 3104 placements, 20 276 placement pins, 6699 nets, 19 546 net pin
  references, 17 244 wiring items, 1231 via-rule records.

**Three fixtures have no dump** — Java hangs on import after
`Opening '<name>.rules'…` and never reaches `DsnWriter`, so no oracle exists and
G1.1 covers 43, not 46:

| fixture | last Java log line |
|---|---|
| `Issue029-hw48na` | `Opening 'Issue029-hw48na.rules'...` |
| `Issue107-freq_teiler_200kHz_kicad` | `Opening 'Issue107-freq_teiler_200kHz_kicad.rules'...` |
| `Issue107-freq_teiler_200kHz_kicad_bad` | `Opening 'Issue107-freq_teiler_200kHz_kicad_bad.rules'...` |

`--all` prints them under the table on every run, so the gap can never silently
turn into "46/46".

### G1.1 — the importer against the oracle

2026-08-28, `--all --ours-dir target/golden` with `fr_io::golden_dump`:
**37/41 clean**, 41 comparable boards (43 dumps − 2 `ORACLE-FAILED`). Per group:

| group | gate | clean | remaining diffs |
|---|---|---|---|
| `resolution` | G1.1 | 41/41 | — |
| `layers` | G1.1 | 41/41 | — |
| `clearance_matrix` | G1.1 | 40/41 | 2 (Issue179) |
| `padstacks` | G1.1 | 40/41 | 1 (Issue179) |
| `components` | G1.1 | 40/41 | 3 (Issue110-RelayModule) |
| `nets` | G1.1 | 41/41 | — |
| `wiring` | G1.1 | 39/41 | 4 (Issue103-Board-Routed, Issue187) |
| `via_rules` | G1.1 | 40/41 | 2 (Issue179) |
| `images` | extra | 40/41 | 4 (Issue110-RelayModule) |
| `net_classes` | extra | 40/41 | 2 (Issue179) |
| `rules_misc` | extra | 41/41 | — |
| `boundary` | extra | 41/41 | — |
| `structure_areas` | extra | 41/41 | — |

The four dirty boards and why:

| fixture | group(s) | cause |
|---|---|---|
| `Issue110-RelayModule` | components, images | the scanner's non-ANSI drop, above — every value matches, only the names differ |
| `Issue179-Autorouter_PCB1_2023-3-24` | clearance_matrix, padstacks, via_rules, net_classes | the empty-identifier loss, above, plus one padstack whose DSN shape is a 2-corner `(polygon 1 0.01 0 -3.5 0 3.5)`: upstream keeps it as an unbounded simplex and writes its corners as `Integer.MAX_VALUE / scale`, we drop the shape |
| `Issue103-Board-Routed`, `Issue187-processor.Z80` | wiring | `fr-board`'s `normalize_traces`, not the importer — the importer inserts exactly the file's 2326 / 2026 traces and the release jar's `normalize_all_traces()` leaves the wire-shape multiset **byte-identical to the input**, while ours combines 61 / 105 traces away after a split/combine oscillation (340 097 successful combines on Issue187 for a net loss of 105) |

`cargo test -p fr-io --release --test golden_gate_test -- --ignored` runs the
same comparison in-process over five boards in ~0.5 s.

## Caveats

- The comparator is exactly as strong as the oracle. A field the release writer
  does not emit (every off-diagonal clearance cell; resolved absolute pin
  centres; the search tree) cannot be gated here no matter how the schema grows.
- `--ours` accepts a DSN dump as well as schema JSON, which is convenient but
  re-introduces the class-pair spelling ambiguity for any writer that does not
  quote reserved characters the way `IdentifierType` does. Emit JSON from the
  Rust side; the DSN path is for A/B-ing two Java dumps.
- Self-comparison proves the parser and the comparator, not the importer. It
  goes green on day one and must stay green; it is a harness gate (G1.0), and
  only `--ours-dir` output moves G1.1.
