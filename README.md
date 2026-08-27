# Fast-Routing: High-Performance PCB Autorouter in Rust

[![Rust](https://img.shields.io/badge/rust-2021%20edition-blue.svg)](https://www.rust-lang.org)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)

A high-performance, modular rewrite of [Freerouting](https://github.com/freerouting/freerouting) in Rust, designed for multi-core CPU parallelism, low-latency execution, and drop-in integration with KiCad, LibrePCB, and Specctra DSN/SES toolchains.

---

## Acknowledgements & Tribute to Freerouting

`fast-routing` stands on the shoulders of giants. We express our deepest respect and gratitude to **Alfons Wirtz**, the original creator of Freerouting, as well as the **Freerouting Open Source Community and Maintainers** who have nurtured and preserved this essential EDA project for over two decades.

Freerouting's mathematical foundations—such as exact 45° planar integer geometry, Minimum Area Tree spatial bounding hierarchies, Bowyer-Watson Delaunay ratsnest generation, and symmetric clearance matrix rules—remain a masterclass in EDA algorithms. 

`fast-routing` pays tribute to this legacy by faithfully re-implementing these exact geometric principles in modern Rust, bringing data parallelism (`Rayon`), zero-cost abstractions, memory safety, and high-throughput execution to hardware designers worldwide.

---

## Key Features

- **Exact 45° Planar Computational Geometry**: High-precision integer coordinates (`CRIT_INT = 33,554,432`) combined with exact rational vector arithmetic to completely eliminate floating-point rounding errors.
- **Multi-Core CPU Parallelism**: Data-parallel candidate route search and batch rip-up & reroute scheduling via `Rayon`, scaling across all available CPU cores.
- **Modular Workspace Crates**: Clean separation of geometric kernels, design rule checkers, spatial data structures, and Specctra parsers.
- **Drop-in Specctra SES/DSN Interoperability**: Full compatibility with KiCad Specctra `.dsn` files and `.ses` session exports.
- **Strict DRC Engine**: Real-time clearance matrix checks supporting per-layer clearance overrides, trace widths, and via rules.

---

## Workspace Architecture

The project is structured into 9 modular workspace crates:

| Crate | Description |
|---|---|
| [`fr-datastructures`](crates/fr-datastructures) | Spatial indexing (`MinAreaTree` BVH), `PlanarDelaunayTriangulation` with Kruskal MST, binary Stein's GCD arithmetic, and `UndoableObjects`. |
| [`fr-geometry`](crates/fr-geometry) | Exact 45° planar integer geometry (`IntPoint`, `IntBox`, `IntOctagon`, `Line`, `Simplex`, `Polyline`, `Direction`). |
| [`fr-board`](crates/fr-board) | PCB representation (`BasicBoard`, `Pin`, `Via`, `PolylineTrace`, `ConductionArea`). |
| [`fr-rules`](crates/fr-rules) | Clearance matrix (`ClearanceMatrix`), Net classes (`NetClass`, `NetClasses`), Padstack, and Via rules. |
| [`fr-io`](crates/fr-io) | Specctra DSN AST parser (`DsnReader`, `DsnLexer`, `DsnDocument`) & Specctra SES serializer (`SesWriter`). |
| [`fr-autoroute`](crates/fr-autoroute) | 45° A* maze router (`MazeSearchAlgo`), multi-pass iterative batch router (`BatchAutorouter`), Rayon parallelism, and CUDA acceleration interface. |
| [`fr-drc`](crates/fr-drc) | Design Rule Checking (`DesignRulesChecker::get_all_clearance_violations`, `NetIncompletes`, `AirLine`). |
| [`fr-core`](crates/fr-core) | Pipeline orchestrator, automated routing job manager (`RoutingJob`), and board statistics. |
| [`fr-cli`](crates/fr-cli) | Command-line interface drop-in replacement for headless PCB autorouting. |

---

## Performance

The performance story is being rebuilt honestly. The previous tables here (DAC2020 timings and a
multi-core scaling curve) were not reproducible from this repository and have been removed.

Current measured state (referee harness in `bench/`, upstream 46-fixture suite, vs the released
Freerouting 2.3.0 jar): the Rust router is fast but routes far less than Freerouting — completion
and DRC quality are being rebuilt first. See `docs/ROADMAP.md` for gates and
`bench/results/` for actual numbers. Reproduce with:

```bash
bench/fetch_fixtures.sh
python3 bench/run_bench.py
```

## Quick Start

### Prerequisites
- [Rust toolchain](https://rustup.rs/) (1.75+ recommended)

### Installation
```bash
# Clone the repository
git clone https://github.com/everyun/fast-routing.git
cd fast-routing

# Build in release mode
cargo build --release -p fr-cli
```

### CLI Usage

```bash
# Autoroute a Specctra .dsn design file with multi-core parallelism:
target/release/fr-cli -de path/to/board.dsn -do path/to/output.ses -mp 5

# CLI Options:
#   -de, --design <FILE>      Input Specctra .dsn design file path (Required)
#   -do, --output <FILE>      Output Specctra .ses session file path (Default: <input>.ses)
#   -mp, --max-passes <N>     Maximum autorouter passes (Default: 10)
#   -us, --threads <N>        Parallel CPU threads for Rayon pool (Default: all logical cores)
#   -v,  --version            Print version information
#   -h,  --help               Print help
```

### Running Tests & Benchmarks

```bash
# Run all workspace unit and integration tests (74 tests)
cargo test --workspace

# Run the comprehensive benchmark suite
cargo run --release --bin benchmark

# Run the multi-chip stress benchmark
cargo run --release --bin stress_benchmark
```

---

## License

This project is licensed under the [GNU General Public License v3.0 (GPL-3.0)](LICENSE) in alignment with upstream Freerouting.
