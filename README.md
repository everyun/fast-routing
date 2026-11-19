# Fast-Routing

A Rust port of [Freerouting](https://github.com/freerouting/freerouting) for
headless CLI (`fr-cli`) and native GPUI interactive (`fr-gui`) PCB
autorouting. The goal is capability parity with the released Freerouting
**2.3.0** jar, then decisively faster wall-clock and lower memory on modern
CPUs. No GPU compute required for routing.

## Acknowledgements & Tribute to Freerouting

`fast-routing` stands on the shoulders of giants. We express our deepest
respect and gratitude to **Alfons Wirtz**, the original creator of Freerouting,
as well as the **Freerouting Open Source Community and Maintainers** who have
nurtured and preserved this essential EDA project for over two decades.

Freerouting's mathematical foundations—such as exact 45° planar integer
geometry, Minimum Area Tree spatial bounding hierarchies, Bowyer-Watson
Delaunay ratsnest generation, and symmetric clearance matrix rules—remain a
masterclass in EDA algorithms.

`fast-routing` pays tribute to this legacy by re-implementing those geometric
principles in Rust.

## The reference

The reference is the released artifact, not a source tree:

- jar: `freerouting-2.3.0.jar` (Build-Revision `2d4de019`)
- router core in that jar is the **v19** algorithm
- IO / DRC / scoring packages in that jar are the **v2** code

See `docs/ARCHITECTURE.md` for the hybrid, `docs/ROADMAP.md` for gates, and
`docs/PARITY-DIVERGENCES.md` for declared differences.

## Installation

### Pre-built binaries

Pre-built binaries and SHA256 checksums are available on the [Releases](https://github.com/everyun/fast-routing/releases) page for:
- **macOS** Apple Silicon (`aarch64-apple-darwin`) & Intel (`x86_64-apple-darwin`)
- **Linux** x86_64 (`x86_64-unknown-linux-gnu`) & ARM64 (`aarch64-unknown-linux-gnu`)
- **Windows** x86_64 (`x86_64-pc-windows-msvc`)

Download and extract the archive for your system, then place `fr-cli` (or `fr-cli.exe` on Windows) in your `PATH`.

### From source (Cargo)

With a Rust 1.75+ toolchain installed:

```bash
# Clone the repository
git clone https://github.com/everyun/fast-routing.git
cd fast-routing

# Install fr-cli into ~/.cargo/bin
cargo install --path crates/fr-cli
```

Or build both CLI and GUI locally without installing:

```bash
cargo build --release -p fr-cli -p fr-gui
# Executables are at target/release/fr-cli and target/release/fr-gui
```

Verify the installation:

```bash
fr-cli --version
```

## Quick start

```bash
# Build the CLI and GPUI GUI
cargo build --release -p fr-cli -p fr-gui

# Autoroute a Specctra .dsn file headlessly
target/release/fr-cli -de path/to/board.dsn -do path/to/output.ses -mp 5

# Open an interactive board viewer / router GUI
target/release/fr-gui --open path/to/board.dsn

# Render a headless board snapshot from the GUI pipeline (<50 MB RSS)
target/release/fr-gui --open path/to/board.dsn --screenshot board.png
```

Flags, Java compatibility, and our extras (`--threads`, `--portfolio`,
`--no-optimizer`, …) are in `docs/CLI.md`. Router knobs are in
`docs/SETTINGS.md`.

## How to evaluate

Quality and time are judged by the referee harness, not by self-reports.

```bash
bench/fetch_fixtures.sh
cargo build --release -p fr-cli
python3 bench/run_bench.py
```

Details, judging rules, and fixture classification: `bench/README.md`.

## Measured results

Headline numbers measured **serially on a quiet machine** (Apple M3 Ultra, 2026-11-19,
commit `99b2ce4`, W=1, `nice -n 10`, J/R interleaved per board, `-mp 20 --timeout 300`)
against a **clean Java reference**: Freerouting 2.3.0 jar (`freerouting-2.3.0.jar`, Build-Revision
`2d4de019`, OpenJDK 26.0.2), run headlessly with fanout + optimizer enabled and a fresh empty
`--user_data_path` for every invocation (hermetic — the shared `$TMPDIR/freerouting/freerouting.json`
is a known measurement contaminant). Quality is judged by the independent referee harness
(`bench/tools/g21_referee` + `g21_judge.py`), never by self-reports. Provenance:
`bench/results/ledger4/20261119-99b2ce4/` (per-board rows in the report tables there).

| metric | value |
|---|---|
| wall-clock geomean, 26 unique `java_finished` designs | **13.84×** vs Java (in-tool geomean **7.86×**; **26 / 26 wins**) |
| wall-clock geomean, all 27 `java_finished` boards | **12.89×** (in-tool **7.47×**; **27 / 27 wins — 0 losses**) |
| total wall-clock, 26 unique designs | Java 753.1 s vs Rust **259.7 s** (**2.90×**); total in-tool 707.0 s vs **259.5 s** (**2.72×**) |
| total wall-clock, all 27 boards | Java 814.7 s vs Rust **290.0 s** (**2.81×**); total in-tool 766.0 s vs **289.8 s** (**2.64×**) |
| peak RSS, 26 unique designs (all 27 boards) | Java 4 846.4 MB vs Rust **82.9 MB** (~58.5× lower); all 27: Java 4 923.6 MB vs Rust **89.3 MB** (~55.1× lower; worst-case 6 955.5 MB vs 565.0 MB, ~12.3×) |
| added violations (judged − unmodified import) | **0** across all 27 `ledger4` boards, all 45/45 judgeable mainline boards, and all 9/9 OOS boards |
| 46-board mainline quality suite (`gate4`, salmon, `99b2ce4`) | **46 / 46 PASS** (`0` added violations across 45/45 judgeable boards); comparative vs Java: **26 / 26 PASS (12 win / 14 tie / 0 loss)**; OOS **9 / 9 PASS** (`bench/results/gate4-quality/20261119-99b2ce4/`) |
| angle violations generated by the router | **0** across all 33 G4-evaluated boards (24 mainline + 9 OOS, `bench/results/g4-gate/99b2ce4/`) |
| KiCad 10.0.5 import | all 8 `.kicad_pcb` out-of-sample boards import cleanly with zero errors and `unconnΔ <= 0` (`bench/results/g4-gate/99b2ce4/`) |

### Former quality & wall-clock gaps closed (`ledger3` → `ledger4`)

Every former quality and speed loss from the `ledger3` (`9b08338`) baseline is resolved in `ledger4` (`99b2ce4`), with **0 quality losses and 0 wall-clock/in-tool losses across all 27 `java_finished` boards**:

| board | Java (ledger4) | Rust ledger3 (`9b08338`) | Rust ledger4 (`99b2ce4`) | wall J/R (ledger4) |
|---|---|---|---|---|
| `Issue039-bug-design` | 179.22 s (`10u / 25v`) | 284.34 s (`30u / 25v`) | **130.84 s** (`4u / 25v`) | **1.37×** (better quality & faster) |
| `Issue102-Mars-64-revE-rot00` | 85.43 s (`0u / 64v`) | 126.38 s (`1u / 64v`) | **38.69 s** (`0u / 64v`) | **2.21×** (0u tie & faster) |
| `Issue110-Pajalnaja_stancija` | 61.52 s (`6u / 18v`) | 97.89 s (`6u / 18v`) | **30.23 s** (`6u / 18v`) | **2.03×** (6u tie & faster) |
| `Issue110-RelayModule` | 9.32 s (`0u / 4v`) | 15.50 s (`0u / 2v`) | **4.05 s** (`0u / 2v`) | **2.30×** (fewer violations & faster) |

Full per-board table (all 27 boards, both tools, RSS, and historical `r1`/`r2`/`r3` comparisons): [`bench/results/ledger4/20261119-99b2ce4/report.md`](bench/results/ledger4/20261119-99b2ce4/report.md).

### Architectural optimization & post-9b08338 changes

The `ledger4` (`99b2ce4`) gains build on exact geometric fast paths with **zero algorithmic divergence** (8/8 deterministic SHA-256 baseline match) plus converged via optimization:

- **Exact $\mathbb{R}^2$ `OctR2` Fourier-Motzkin overlap & cached tree hulls**: replaces hot-loop `Simplex::enlarge`, `remove_redundant_from_sorted`, `board_bounds` scans, and SAT fallback on 45°/90° geometry in pull-tight, shove, and room completion with stack-allocated `i64` intervals while matching exact simplex semantics bit-for-bit.
- **Stack `FastPoint` & cached `CompletedShape::oct_r2`**: eliminates `BigInt` heap allocations on `restrain_shape` corner/segment intersections and room overlap tests.
- **Deflected neckdown angle-knee guard (`insert_angle_compliant_neckdown_segment`)**: bridges off-axis `spring_over`/`pull_tight` neckdown stubs via `calculate_additional_corner`, bringing router-generated G4 angle violations to **0 / 33 boards** (including `cla45-sensor-l6`).
- **`RouterObserver`, fast board snapshot, and native GPUI GUI (`crates/fr-gui`)**: live routing canvas, layer switcher, ratsnest inspector, and headless `--screenshot` renderer (<50 MB RSS).

Previously landed convergence changes (provenance: `bench/results/main-sweeps/`, `bench/results/ledger4/20261119-99b2ce4/`):

- **Fallback-via clearance-class fix + board-via fallback default-off** (`d8cc31c`, `3bb7576`):
  the fallback via uses the net's via clearance class (DSN net-class semantics, zero added
  violations), and board-level via fallback ships disabled by default per the tree-desync
  ruling (upstream's fallback is self-consistently wrong: `RoutingBoard.java:1019-1035` assigns
  the default class on read-back; `DECISIONS.md` 2026-09-07 01:00/01:40). Accepted cost:
  Issue039 default mode **30 → ~36u**; re-decided when Full becomes default.
- **Fanout stop rules aligned upstream** (`007d0bb`, `c94876d`): our fanout no-progress stop was
  removed (the "doesn't lose coverage" argument was falsified by the RelayModule trace),
  restoring upstream's three stop rules. Win: **Issue219 361 → 299u**; accepted costs:
  sensor-l6 110 → 119u, power-io 60 → 74u (`DECISIONS.md` 2026-09-07 03:25).
- **Issue039 under a job deadline — measured, not the same as budgetless quality.** Serial
  salmon, `-mp 20 --job-timeout 285`, default mode (integrator's merge records,
  `REPORT-integrate.md`): `9b08338` ≈ **30u/25v** → current mainline (`ece1508`) **40u/25v**.
  Cause: with the no-progress stop removed, fanout escapes more pins (606 → 729) and takes
  longer, so only 6 passes finish before the deadline — a **budget-allocation problem, not a
  search regression**: without a deadline, deterministic 20-pass runs on the current mainline
  are **4u better** than `9b08338`. Landed (`2a26e1a`): the fanout stage's share of the job
  budget is capped at **25%** (`FR_FANOUT_BUDGET_PCT` overrides; no deadline — no cap, and
  no-deadline deterministic runs stay byte-identical).
- **FANOUT_VIA predicate kept as the stricter own version** (revert of the upstream
  `is_fanout_via` port, `PARITY-DIVERGENCES.md`): a declared intentional divergence that holds
  **Issue102 at 1u** (the upstream predicate had regressed it to 13u in the orca sweep through
  slower convergence; `DECISIONS.md` 2026-09-07 05:35/06:35).
- **45° family tree keeps trace/outline strips at their base Simplex** (`d89ded9`): recorded in
  `docs/PARITY-DIVERGENCES.md`; Issue054 stays at **4.7 s / 0u**.
- **OptViaAlgo via location optimization during pull-tight** (`767283d`, `fdc0259`): ported
  upstream's `OptViaAlgo` (`REPORT-optvia.md`, `DECISIONS.md` 2026-09-08 20:20). Win:
  **Issue039 deterministic 20-pass converges to 4u/25v** (Java reference: 10u), and 285s deadline
  improves to **24u** (from 39u); `cla45-power-io-l6` 285s improves to **64u** (from 74u);
  `cla45-sensor-l6` improves to **113u** (from 115u). Accepted costs: `kicad-coldfire-unrouted`
  285s 9u → 19u (opt-via work makes passes slightly more expensive, finishing fewer passes before
  cutoff), `easyeda-pcb1` 8u → 9u, and `Issue102` under Full mode 285s 0u → 1u (serial det stays
  **0u board complete**); Doors 285 s Issue102 0u → 2u (det still 0u at pass 17 vs 11; fix in
  progress, TASK-102-deadline).

### Parallel routing

`--parallel-connections` defaults to **1**: it is the only mode that is bit-for-bit deterministic
and referee-clean everywhere. Measured applicability of `--parallel-connections 8`
(REPORT-t3, orca): **2.3–3.1× route-stage speedup on sparse large boards**
(Issue230, Issue157, Issue269), with 24-board + 9-OOS acceptance PASS (0 worse, 0 added
violations) — but on pin-dense congested boards corridor pre-emption makes parallel search
unequal to serial greedy allocation and can break convergence, so the default stays 1.

The 45° router mode is a **runtime** choice via the environment variable `FR_ROUTER_TILING_45=doors|full|off` (default `doors`): the `full` value selects the
unified full-tiling chain, which matches Java's per-expansion cost on the hardest boards
(Issue039 ~35.7 µs/exp ≈ Java 36) and wins on multi-layer boards under budget
(sensor-l6 119u vs Doors 123u), but still regresses a few congested boards
(e.g. RelayModule 0→2u), so **Doors remains the default** until it is zero-regression
(`docs/PARITY-DIVERGENCES.md`, REPORT-tiling45-default.md).

### How to reproduce

```bash
# 1. Build release binaries
cargo build --release -p fr-cli -p g21-referee

# 2. Serial ledger (quiet machine required; numbers above are W=1, nice -n 10)
python3 bench/run_bench.py --serial-perf --passes 20 --timeout 300 --concurrency 1

# 3. One-touch quality acceptance vs the recorded mainline sweep
#    (runs remotely on orca/salmon/whale, auto-picks the least loaded host,
#     compares same-doors-mode against bench/results/main-sweeps/)
bench/tools/remote/accept.sh <worktree-or-commit> --suite 24+oos --doors default
```

Remote test machines (quality sweeps only; serial timings are always taken on a quiet Mac):
- **orca** — 80-core Debian 13
- **salmon** — EPYC 32c/64t (fastest single-core, use for deadline-sensitive suites)
- **whale** — old Xeon, non-deadline boards and byte-identity checks only

Addresses are in the local ops notes.

## License

This project is licensed under the [GNU General Public License v3.0
(GPL-3.0)](LICENSE) in alignment with upstream Freerouting.
