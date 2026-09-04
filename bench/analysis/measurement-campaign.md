# M0.5 measurement campaign — consolidated results

Every number below is parsed out of a log in `bench/results/`. Nothing here is
estimated, extrapolated, or scaled. Regenerate the tables with:

```
python3 bench/analysis/campaign.py              # all sections
python3 bench/analysis/campaign.py --section serialgc
python3 bench/analysis/test_campaign.py         # guards the claims below
```

The per-board numbers are also folded into `bench/fixtures-manifest.json`
(`fixtures[].uncapped`, `coverage.by_uncapped_status`) so gates read them from
data rather than from this document.

## 0. Provenance

| campaign | when | config | logs |
|---|---|---|---|
| capped baseline | 2026-08-28 01:27–03:17 | `-mp 20`, 300 s cap, one board at a time | `bench/results/20260828-013025/` |
| uncapped | 05:13–08:33 | `-mp 20`, `timeout 7200`, **4 concurrent workers** | `bench/results/java-uncapped/*.log` |
| SerialGC | 05:03–05:22 | `-XX:+UseSerialGC -mp 20`, 600 s cap, serial | `bench/results/java-experiments/*.serialgc.log` |
| DEBUG console | 05:27–05:39 | `-Dfreerouting.logging.console.level=DEBUG -mp 20` | `bench/results/java-experiments/*.profile.log` |
| 3-run stability | 2026-08-29 03:30–08:0x | the capped baseline's command line, three times per fixture, serial, `nice -n 15` | `bench/results/java-stability/<stem>.r{1,2,3}.log` |

Same jar throughout: `/tmp/frbase/freerouting-2.3.0.jar` (v2.3.0, build-date
2026-08-07), same JDK (Homebrew OpenJDK 26.0.2), same machine.

Three things about how the campaign actually executed, all load-bearing:

- **`measure.sh` phase 1 never ran.** `/tmp/frbase/measure.log` records
  `xargs: command line cannot be assembled, too long`, so the uncapped sweep
  was re-launched by hand as `run_one_uncapped.sh` workers *after* phase 2 had
  already started. That is why the SerialGC runs are contaminated (§2) and why
  the uncapped runs overlapped each other four at a time.
- **`slow6.txt` has six entries; five ran.** Its last line has no trailing
  newline, so phase 2's `while read -r` dropped `Issue054-tairakb.dsn`
  silently. There is no SerialGC datum for that board.
- **The DEBUG-console phase produced no DEBUG output at all** — 0 `DEBUG`
  lines across all five logs; `-Dfreerouting.logging.console.level` never
  reaches `LoggingSettings`. It therefore contributes nothing to the intended
  stage split, which remains unmeasured. What it does supply is a second
  *default*-collector run of each of the five boards, in the same phase and
  under the same machine load as its SerialGC run — that is the control §2
  leans on.

## 1. Uncapped Java — `T_java(complete)`

The 14 boards the 300 s baseline could not finish, re-run with a 2 h cap. The
`status` column is what decides whether the wall time may be cited:
`completed` is `T_java(complete)`; `hit_cap` is a lower bound; `blocked` does
not measure the board at all (§4).

| fixture | wall s | stage times s | passes | score | unrouted | violations | hit 2 h cap | status | rust wall (300 s run) |
|---|---|---|---|---|---|---|---|---|---|
| Issue015-StackOverflow | 459.75 | auto-routing 447.39 | 20 | 699.61 | 54 | 511 | no | completed | 5.62 s |
| Issue022-AutoRouter_interrupted | 832.30 | auto-routing 820.78 | 20 | 731.52 | 42 | 531 | no | completed | 2.00 s |
| Issue027-zMRETestFixture | 804.34 | auto-routing 791.86 | 20 | 782.37 | 7 | 1138 | no | completed | 2.58 s |
| Issue029-hw48na | 7200.39 | - | - | - | - | - | **yes** | blocked | 8.06 s |
| Issue066-Project_GP8B | 850.22 | auto-routing 838.30 | 18 | 987.33 | 5 | 23 | no | completed | 16.83 s |
| Issue070-Autorouter_FQ101_PCB_2022-05-13 | 4405.40 | auto-routing 4394.75 | 18 | 590.24 | 113 | 275 | no | completed | 0.01 s |
| Issue103-Board-Unrouted | 885.15 | auto-routing 875.36 | 20 | 977.18 | 4 | 60 | no | completed | 5.89 s |
| Issue107-freq_teiler_200kHz_kicad_bad | 7200.39 | - | - | - | - | - | **yes** | blocked | 0.38 s |
| Issue113-Protein | 1709.19 | auto-routing 1698.84 | 20 | 932.86 | 21 | 44 | no | completed | 5.91 s |
| Issue145-smoothieboard | 1052.70 | auto-routing 1039.77 | 20 | 801.50 | 135 | 112 | no | completed | 14.56 s |
| Issue178-KeebMaker_Sofle_Choc | 296.21 | auto-routing 285.53 | 18 | 976.50 | 6 | 20 | no | completed | 5.44 s |
| Issue208-freerouting | 7200.38 | - | - | - | - | - | **yes** | hit_cap | 0.74 s |
| Issue214-freerouting | 1087.08 | auto-routing 1073.50 | 18 | 752.05 | 64 | 192 | no | completed | 8.74 s |
| Issue219-LogicBoard_smt | 4675.43 | auto-routing 4664.55 | 19 | 997.96 | 2 | 0 | no | completed | 30.86 s |

11 / 14 completed. Completed wall sums to 17 057.77 s (max 4675.43 s); the
whole campaign, capped runs included, burned 38 658.93 s = 10.74 h.

Reading:

- **No board converged.** 6 of the 11 stopped on the score stall rule ("has
  not improved by more than 0.5 points in the last 10 passes") — Issue015 at
  pass 20, Issue066/070/178/214 at 18, Issue219 at 19. The other 5 simply
  exhausted `-mp 20` with the stall rule never firing. None reached 0
  unrouted; best is Issue219 at 2 unrouted / 0 violations, worst Issue145 at
  135 / 112.
- **`-mp 20`, not the 300 s cap, is what bounds 5 of these boards.** They do
  not "need more time to finish" — they need more passes, and more passes is
  not obviously convergent: Issue015 oscillates between 54 and 67 unrouted
  from pass #2 onward and its stall rule fires against pass #10's score.
- **Suite-level `T_java(complete)` is now known for 42 / 46 boards:**
  1632.7 s (31 boards under the 300 s baseline) + 17 057.8 s (11 uncapped)
  = 18 690.5 s = 5.19 h. Still unknown: Issue179 (crash), Issue208 (2 h cap),
  Issue029 and Issue107-bad (blocked, §4).
- **Issue208 is the new pathological board.** 7200 s and pass #1 never
  finished, on a board with only 378 unrouted items. The 300 s baseline
  recorded it as one more timeout; it is in a different class from the others
  and needs its own investigation before any gate references it.
- **These walls carry a small, bounded concurrency tax.** Every uncapped run
  shared the machine with 2–8 other live JVMs (below); at ~1.15 cores each
  (§"GC surplus") the worst moment is ~10 of 28 cores on an M3 Ultra with
  256 GB, so the perturbation is real but small, and it is an inflation —
  these are upper bounds on `T_java(complete)`, never under-reports.

| run | start | end (start + wall) | status | overlapping live runs |
|---|---|---|---|---|
| Issue015-StackOverflow | 05:13:16 | 05:20:55 | completed | 2 |
| Issue022-AutoRouter_interrupted | 05:13:16 | 05:27:08 | completed | 4 |
| Issue027-zMRETestFixture | 05:13:16 | 05:26:40 | completed | 3 |
| Issue029-hw48na | 05:13:16 | 07:13:16 | blocked | 12 |
| Issue066-Project_GP8B | 05:20:56 | 05:35:06 | completed | 4 |
| Issue070-Autorouter_FQ101_PCB_2022-05-13 | 05:26:41 | 06:40:06 | completed | 8 |
| Issue103-Board-Unrouted | 05:27:09 | 05:41:54 | completed | 3 |
| Issue107-freq_teiler_200kHz_kicad_bad | 05:35:06 | 07:35:06 | blocked | 9 |
| Issue113-Protein | 05:41:54 | 06:10:23 | completed | 3 |
| Issue145-smoothieboard | 06:10:23 | 06:27:55 | completed | 2 |
| Issue178-KeebMaker_Sofle_Choc | 06:27:56 | 06:32:52 | completed | 2 |
| Issue208-freerouting | 06:32:52 | 08:32:52 | hit_cap | 4 |
| Issue214-freerouting | 06:40:06 | 06:58:13 | completed | 3 |
| Issue219-LogicBoard_smt | 06:58:13 | 08:16:08 | completed | 2 |

- **The Rust column is not a speedup claim.** On 10 of the 11 completed
  boards our current router leaves *more* nets unrouted than Java (e.g.
  Issue219: Java 2 unrouted, ours 297), and on the 11th (Issue070) our 0.01 s
  "win" is a bail-out with 27 violations. A geomean over these rows computes
  to 269.5×, and G2.1's quality filter rejects all 11 of them, so the number
  is meaningless today. It is recorded here only so nobody recomputes it and
  believes it.

### GC surplus on the critical path

`/usr/bin/time` against a router that logs "on 1 thread":

| fixture | real s | user s | sys s | user/real | max RSS MB |
|---|---|---|---|---|---|
| Issue015-StackOverflow | 459.75 | 517.38 | 31.30 | 1.125 | 3541 |
| Issue022-AutoRouter_interrupted | 832.30 | 941.97 | 56.34 | 1.132 | 4012 |
| Issue027-zMRETestFixture | 804.34 | 902.68 | 58.46 | 1.122 | 3872 |
| Issue066-Project_GP8B | 850.22 | 987.72 | 84.22 | 1.162 | 6422 |
| Issue070-Autorouter_FQ101_PCB_2022-05-13 | 4405.40 | 4681.60 | 216.24 | 1.063 | 9759 |
| Issue103-Board-Unrouted | 885.15 | 1037.11 | 86.03 | 1.172 | 7831 |
| Issue113-Protein | 1709.19 | 1923.75 | 142.84 | 1.126 | 4983 |
| Issue145-smoothieboard | 1052.70 | 1270.60 | 185.04 | 1.207 | 6724 |
| Issue178-KeebMaker_Sofle_Choc | 296.21 | 377.88 | 54.21 | 1.276 | 4298 |
| Issue214-freerouting | 1087.08 | 1208.11 | 94.84 | 1.111 | 4105 |
| Issue219-LogicBoard_smt | 4675.43 | 5163.07 | 423.38 | 1.104 | 7895 |

user/real 1.063–1.276, mean 1.145. On long runs the JIT has amortized away,
so this surplus is essentially the parallel collector: 6–28 % of a core's
worth of CPU spent *beside* the router, not in front of it. (The 300 s
baseline's "~40 % of process CPU in GC/JIT" figure quoted in ARCHITECTURE is
dominated by JIT warm-up on short runs and does not transfer to these.)

## 2. SerialGC — direction only, magnitudes contaminated

**Caveat, stated before the numbers.** Phase 2 was supposed to run on a quiet
machine. It did not: only the first two SerialGC runs (Issue209, Issue190)
finished before the hand-launched uncapped workers started at 05:13:16. The
other three shared the machine with 3–4 live JVMs, and every DEBUG-console run
did. So read the `SerialGC vs baseline` column as a magnitude on those two
rows only — and of those two, only Issue190 also has a comparable outcome —
and as direction everywhere else.

| fixture | baseline fanout s | baseline auto-route s | SerialGC auto-route s | DEBUG-console auto-route s | SerialGC vs DEBUG-console | SerialGC vs baseline | peak heap MB base/serial | alloc GB base/serial | same outcome as baseline | concurrent uncapped workers |
|---|---|---|---|---|---|---|---|---|---|---|
| Issue209-split05 | 0.78 | 279.82 | 280.24 | 272.97 | +2.7% | +0.2% | 1876 / 1474 | 1854 / 1642 | **no** | 0 |
| Issue190-processor.Z80 | (none) | 250.38 | 265.37 | 255.36 | +3.9% | +6.0% | 2584 / 1010 | 3264 / 3276 | yes | 0 |
| Issue039-bug-design | 3.62 | 157.24 | 209.05 | 182.30 | +14.7% | +32.9% | 2273 / 1240 | 1513 / 1654 | **no** | 3 |
| Issue269-caniot-tiny-arm | 6.70 | 151.81 | 217.61 | 196.53 | +10.7% | +43.3% | 4848 / 478 | 2063 / 2808 | **no** | 3 |
| Issue157-TeamAdapt-LinePCB | 6.75 | 83.03 | 90.67 | 77.26 | +17.4% | +9.2% | 2160 / 810 | 862 / 717 | **no** | 4 |

Only the auto-routing stage is compared: the baseline ran a fanout stage on
four of these five boards and the 05:03+ campaign ran it on none (§5), so
stage *sums* would compare different pipelines. Even the auto-routing stage
starts from a different board wherever fanout fired — which is exactly why
four of five rows end with a different unrouted count and only
**Issue190-processor.Z80** is a true apples-to-apples pair: no fanout on
either side, both clean of concurrency, identical 18 passes / 12 unrouted /
1372 violations / score 656.58.

Conclusion, on that one clean pair plus the direction of the other four:

1. **Forcing single-threaded collection costs +6.0 % wall on the clean row**
   (+2.7 % to +17.4 % against the DEBUG-console runs of the same phase, which
   are the same pipeline and differ only in collector). The parallel collector
   is buying a single-digit-percent wall improvement, i.e. that is the whole
   size of the GC work currently sitting *on* the critical path.
2. **The mutator does the same work either way.** Self-reported total
   allocation is unchanged by the collector — Issue190 3264 GB (default) vs
   3276 GB (SerialGC), +0.4 % — and the routing outcome is bit-for-bit the
   same decision sequence. SerialGC is not changing what the router computes;
   it is changing who pays for the garbage.
3. **The heap is deferred garbage, not live data.** Peak heap drops 2.6× on
   Issue190 (2584 → 1010 MB) and 10.1× on Issue269 (4848 → 478 MB) when the
   collector is forced to keep up. The multi-GB RSS in the baseline table is
   the default collector choosing not to collect, not a working set.

So: **allocation elimination alone is not the speedup story.** Java allocates
0.7–21 TB per run across this campaign, and the machine (Apple M3 Ultra,
`hw.ncpu` = 28, 256 GB) absorbs almost all of it on cores the router is not
using. Removing every allocation recovers the single-digit percent in (1) plus
the TLAB-zeroing cost bounded at 1.1–1.4× in `perf-forensics.md` — not a
10×. This is the same conclusion ARCHITECTURE v3 reaches from the off-CPU
measurement ("kill allocation → 10× is wrong as stated"), reached
independently from the collector side, and it keeps the levers ranked as
ARCHITECTURE has them: recompute elimination first, constant factor second,
allocation third.

### Concurrency ledger

| run | start | end | live uncapped workers | which |
|---|---|---|---|---|
| Issue209-split05.serialgc | 05:03:48 | 05:08:37 | 0 | (none) |
| Issue209-split05.profile | 05:22:25 | 05:27:06 | 4 | Issue022-AutoRouter_interrupted, Issue027-zMRETestFixture, Issue066-Project_GP8B, Issue070-Autorouter_FQ101_PCB_2022-05-13 |
| Issue190-processor.Z80.serialgc | 05:08:38 | 05:13:15 | 0 | (none) |
| Issue190-processor.Z80.profile | 05:27:07 | 05:31:33 | 4 | Issue022-AutoRouter_interrupted, Issue066-Project_GP8B, Issue070-Autorouter_FQ101_PCB_2022-05-13, Issue103-Board-Unrouted |
| Issue039-bug-design.serialgc | 05:13:16 | 05:16:54 | 3 | Issue015-StackOverflow, Issue022-AutoRouter_interrupted, Issue027-zMRETestFixture |
| Issue039-bug-design.profile | 05:31:34 | 05:34:46 | 3 | Issue066-Project_GP8B, Issue070-Autorouter_FQ101_PCB_2022-05-13, Issue103-Board-Unrouted |
| Issue269-caniot-tiny-arm.serialgc | 05:16:55 | 05:20:43 | 3 | Issue015-StackOverflow, Issue022-AutoRouter_interrupted, Issue027-zMRETestFixture |
| Issue269-caniot-tiny-arm.profile | 05:34:47 | 05:38:15 | 3 | Issue066-Project_GP8B, Issue070-Autorouter_FQ101_PCB_2022-05-13, Issue103-Board-Unrouted |
| Issue157-TeamAdapt-LinePCB.serialgc | 05:20:44 | 05:22:24 | 4 | Issue015-StackOverflow, Issue022-AutoRouter_interrupted, Issue027-zMRETestFixture, Issue066-Project_GP8B |
| Issue157-TeamAdapt-LinePCB.profile | 05:38:15 | 05:39:42 | 2 | Issue070-Autorouter_FQ101_PCB_2022-05-13, Issue103-Board-Unrouted |

(`blocked` uncapped runs are excluded — they sat on a modal dialog at ~0 % CPU,
present on the process table but not on the machine.)

## 3. Issue219 — the fanout non-convergence, quantified

Source: `bench/results/20260828-013025/logs/Issue219-LogicBoard_smt.java.stdout.txt`
(the 300 s baseline, the only Issue219 run in which the fanout stage executed
at all — see §5).

| fanout pass | s | SMD pins fanouted | not routed | extra vias | board hash |
|---|---|---|---|---|---|
| 1 | 89.74 | 1231 | 1 | +853 | d441014b |
| 2 | 17.83 | 270 | 0 | +124 | c056fbdc |
| 3 | 11.09 | 105 | 0 | +22 | b540af67 |
| 4 | 9.37 | 85 | 0 | +9 | d86412ac |
| 5 | 7.49 | 64 | 0 | +6 | 9940307b |
| 6 | 7.38 | 59 | 0 | +2 | 9b092614 |
| 7 | 7.40 | 54 | 0 | +0 | 5e2707a7 |
| 8 | 7.44 | 55 | 0 | +0 | 17979d2e |
| 9 | 7.32 | 51 | 0 | +2 | ec1355b0 |
| 10 | 7.29 | 48 | 0 | +4 | 94aa340d |
| 11 | 7.16 | 44 | 0 | +0 | b3fc27e9 |
| 12 | 7.08 | 43 | 0 | +0 | 7596c9da |
| 13 | 7.12 | 44 | 0 | +0 | 7a24bd16 |
| 14 | 7.06 | 43 | 0 | +0 | a19d1d8a |
| 15 | 7.11 | 44 | 0 | +0 | 60b17983 |
| 16 | 7.10 | 43 | 0 | +0 | 3b716651 |
| 17 | 7.09 | 44 | 0 | +0 | 94f5f27a |
| 18 | 7.09 | 43 | 0 | +0 | 2389369c |
| 19 | 7.13 | 44 | 0 | +0 | d3ad898d |
| 20 | 7.12 | 43 | 0 | +0 | 10566b90 |

Stage line: 1260 SMD pins in, **245.90 s**, escaped 1211/1260 (96.1 %),
243.77 CPU s, 1464.55 GB allocated. Pass times sum to 243.41 s.

- **Contiguous no-progress tail: passes #11–#20 — 10 of 20 passes, 71.06 s =
  28.9 % of the stage.** Each re-fanouts 43–44 SMD pins, adds **+0** extra
  vias, and produces a *different* board hash: the pass is rewriting escapes
  it already had, at escalating ripup costs (1100 → 2000), and converging on
  nothing.
- Widened to the last productive pass: passes #1–#2 place 977 of the 1022
  extra vias (95.6 %) in 107.57 s; the remaining **18 passes spend 135.84 s
  (55.2 % of the stage) to place the other 45**. Passes #7–#20 — the 14-pass
  window ARCHITECTURE already cites — cost 100.51 s for +6 vias.
- **The loop has no stall detector.** Its exit test is "no pin was
  (re-)fanouted this pass", which is why Issue178 stops after 3 passes when
  pass #3 fanouts 0 pins, and why Issue219 — which keeps successfully
  re-fanouting the same 43–44 pins forever — runs to the `-mp` cap every time.
  Compare the auto-router, which *does* have one: in the uncapped Issue219 run
  its score froze at 997.96 after pass #9 and the stall rule fired 10 passes
  later, capping the waste at 234.89 s = 5.0 % of a 4664.55 s stage.

Port implication: a "no net progress" exit on the fanout loop is worth 28.9 %
of that stage on this board. It is **not** free of parity risk, though — those
passes add no escape vias but they do rewrite existing escapes (a fresh board
hash every pass), so they hand a different board to the auto-router. The stop
rule needs the same treatment as any other divergence from upstream: an
equivalence gate on the final routed result, not an assertion that skipping
them is a no-op.

## 4. Issue029 and Issue107-bad never routed — modal dialog, not slow routing

Both logs stop at `Opening '<board>.rules'...` with the 2 h cap consuming
12.29 s and 8.77 s of user CPU respectively. A `jstack` of a reproduction
(same jar, same flags, `Issue107-freq_teiler_200kHz_kicad.dsn`) shows the main
thread parked here:

```
java.awt.Dialog.show(Dialog.java:1044)
app.freerouting.gui.WindowMessage.confirm(WindowMessage.java:79)
app.freerouting.gui.GuiManager.read_rules_file(GuiManager.java:459)
app.freerouting.gui.GuiManager.create_board_frame(GuiManager.java:420)
app.freerouting.gui.GuiManager.InitializeGUI(GuiManager.java:123)
app.freerouting.Freerouting.main(Freerouting.java:1075)
```

`GuiManager.read_rules_file` opens `<board>.rules` next to the DSN and, when
that file exists, pops a modal confirmation before reading it. In a batch
context nobody answers, so the process blocks until the harness kills it. The
upstream fixture set ships five such sidecars —
`Issue029-hw48na{,_valid,_invalid}.rules` and
`Issue107-freq_teiler_200kHz_kicad{,_bad}.rules` — covering three fixtures.

Consequences:

- These are **not** DSN-import bugs and not routing timeouts. ARCHITECTURE's
  "two fixtures hang in DSN import before routing" should read "block on a
  `.rules` confirmation dialog before routing".
- Their `7200.39 s` measures the harness timeout. `uncapped.status ==
  "blocked"` in the manifest exists to stop anyone citing it.
- **The trap is not confined to those two boards.** The reproduction above
  used `Issue107-freq_teiler_200kHz_kicad.dsn` — the *good* variant, which
  routed in 14.09 s under the 300 s baseline — and it blocked at the same
  call, >5 min at 0.1 % CPU, until killed. Whatever selects the GUI path is
  not a property of the board, so any board with a matching `.rules` sidecar
  can lose its whole budget on a future run.
- Mitigation before M0.5's `-mp 9999` sweep: run headless
  (`-Djava.awt.headless=true`) or move the sidecars aside, and assert in the
  runner that a board which produced no stage-started line is reported as a
  harness failure rather than a slow board. Tracked in ROADMAP M0.5.

## 5. Open confound: the whole 05:03+ campaign ran with no fanout stage

The 300 s baseline ran a fanout stage on **37 of 46** boards. Every run in the
05:03–08:33 campaign — all 14 uncapped, all 5 SerialGC, all 5 DEBUG-console —
ran **zero**, with the same jar and the same `-mp 20 -l en`. Issue219 is the
clearest instance: baseline `Fanout stage started ... 1234 of 1286 SMD pins`
then `Auto-routing stage started ... for 603 unrouted items`; uncapped goes
straight to `Auto-routing stage started ... for 981 unrouted items` on the
same board hash `8bebeef9db56d949b408efd4eeb0cc3d`.

Root cause not established. `FanoutSettings.enabled` is a nullable field
resolved through `SettingsMerger` from bundled defaults, a `freerouting.json`,
and `FREEROUTING__`-prefixed environment variables; no such file or variable
exists on the machine now, and the GitHub-rate-limit warning present in the
later logs appears in only 4 of 14, so it does not correlate.

What it means for the numbers here:

- The uncapped walls in §1 are `T_java(complete)` **for the auto-routing-only
  pipeline**. Where fanout normally fires it hands the auto-router a different
  and usually easier board (Issue219: 603 unrouted items instead of 981), so
  the §1 walls are not the same quantity as a full-pipeline run — they are
  neither reliably above nor below it, and must be re-measured with fanout
  pinned before they are used as a two-sided bound.
- §2's baseline-vs-SerialGC column crosses this boundary on 4 of 5 rows, which
  is the second reason (after concurrency) that only Issue190 is quotable.
- The `-mp 9999` re-baseline must pin fanout explicitly
  (`--router.fanout.enabled=…`) and record the resolved value, or the two
  halves of the reference will keep silently disagreeing.

`bench/analysis/test_campaign.py::test_no_uncapped_run_executed_a_fanout_stage`
pins the observation so a re-run that changes it fails loudly instead of
quietly invalidating this document.

## 6. 3-run stability — which boards may be judged comparatively

M0.5's "record 3-run stability per fixture", run as the capped baseline's own
command line three times over each of the 46 boards, serially, `nice -n 15`:

```sh
# Prefer the harness (pins the jar, isolates --user_data_path per call, W=4):
python3 bench/run_java_stability.py --workers 4 --rounds 3 --passes 20 --timeout 300

# Equivalent shell shape, do not omit --user_data_path= (see PARITY-DIVERGENCES Optimizer):
for f in /tmp/freerouting-upstream/fixtures/*.dsn; do
  b=$(basename "$f" .dsn)
  for i in 1 2 3; do
    udp=$(mktemp -d)
    nice -n 10 /opt/homebrew/opt/openjdk/bin/java -Djava.awt.headless=true \
      -jar /tmp/frbase/freerouting-2.3.0.jar \
      -de "$f" -do "bench/results/java-stability/$b.r$i.ses" -mp 20 -l en \
      --user_data_path="$udp" \
      > "bench/results/java-stability/$b.r$i.log" 2>&1
  done
done
```

`manifest.parse_stability_run` / `summarise_stability` fold the logs into
`fixtures[].stability`; the classification is defined in `bench/README.md`
(manifest question 4) and is what ROADMAP G2.1 reads to choose a gate.

All 46 fixtures swept, 138 runs: **32 `stable`, 14 `no_result`, 0 `unstable`**
(`python3 bench/analysis/campaign.py --section stability`). No board changed its
answer between runs — every fixture that reached a final score reached the same
`(unrouted, violations)` all three times — so every one of the 29
`java_finished` boards is comparative-gate eligible. That is a real result and
not a foregone one: the jar's batch loop is wall-clock-budgeted, so a board that
finishes near the cap could easily have landed on different passes in different
runs. None did, on this machine, at this load — which is also the caveat, since
a busier machine is exactly the condition under which the budget would bite.

The rest split as the other two campaigns already implied: boards Java is
killed on at 300 s report nothing at all, three times over, and `Issue006`
never starts a routing stage because it is not a board. Both are `no_result` —
recorded as "the runs agree there is nothing to report" rather than as a
missing measurement, because that distinction is exactly what the G2.1
anti-degeneracy guard spends (`bench/README.md`, "Judging a session").

The one number worth keeping from a `no_result` log is Java's own
`started with N unrouted` line, which survives even when the run is killed:
paired with a final count from the same tool it is the only self-consistent way
to ask "did the reference make progress on this board?".
