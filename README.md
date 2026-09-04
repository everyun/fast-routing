# Fast-Routing

A Rust port of [Freerouting](https://github.com/freerouting/freerouting) for
headless PCB autorouting. The goal is capability parity with the released
Freerouting **2.3.0** jar, then faster wall-clock and lower memory on
multi-core CPUs. No GPU.

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

## Quick start

```bash
# Build the CLI
cargo build --release -p fr-cli

# Autoroute a Specctra .dsn file
target/release/fr-cli -de path/to/board.dsn -do path/to/output.ses -mp 5
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

Numbers below are filled by the gate task from a quiet-machine run of
`python3 bench/run_bench.py`. Until then they are placeholders.

| metric | value |
|---|---|
| geomean wall-clock, boards both tools finish | `<<GATE:both-finished-geomean>>` |
| JVM startup (broken out of Java wall-clock) | `<<GATE:jvm-startup>>` |
| unrouted (same boards) | `<<GATE:both-finished-unrouted>>` |
| added violations (judged − import) | `<<GATE:both-finished-added-violations>>` |
| angle violations | `<<GATE:both-finished-angle-violations>>` |
| KiCad DRC delta | `<<GATE:both-finished-kicad-drc-delta>>` |

## License

This project is licensed under the [GNU General Public License v3.0
(GPL-3.0)](LICENSE) in alignment with upstream Freerouting.
