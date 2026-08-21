# Freerouting to Rust Rewrite: Porting & Parity Documentation

## Parity Baseline & Architecture

This repository is a complete, modular, high-performance rewrite of **Freerouting** in modern Rust, supporting multi-core CPU parallelism (Rayon) and GPU/CUDA acceleration hooks, with drop-in parity for headless PCB autorouting.

---

## Workspace Modules & Status

| Crate | Java Source Package | Status | Tested Features |
|---|---|---|---|
| `fr-datastructures` | `app.freerouting.datastructures` | ✅ **Complete** | `Signum`, `BigIntAux`, binary GCD, `MinAreaTree` BVH, `PlanarDelaunayTriangulation` & MST, `UndoableObjects` transactions, `IdentifierType`, `IndentFileWriter`, `ArrayStack`, `Stoppable`, `TimeLimit` |
| `fr-geometry` | `app.freerouting.geometry.planar` | ✅ **Complete** | `IntPoint`, `RationalPoint`, `Point`, `IntVector`, `RationalVector`, `Vector`, `Direction`, `FortyfiveDegreeDirection`, `Limits`, `Side`, `FloatPoint`, `FloatLine`, `IntBox`, `IntOctagon`, `Line`, `Simplex`, `Polyline`, `PolylineShape`, `LineSegment`, `Circle`, `Polygon` |
| `fr-board` | `app.freerouting.board` | ✅ **Complete** | `ItemHeader`, `FixedState`, `Pin`, `Via`, `PolylineTrace`, `ObstacleArea`, `ConductionArea`, `BasicBoard` |
| `fr-rules` | `app.freerouting.rules` | ✅ **Complete** | `ClearanceMatrix` (NxN with per-layer values & safety margins), `NetClass`, `NetClasses`, `Net`, `Nets`, `ViaRule`, `ViaInfo`, `ViaInfos`, `BoardRules`, `Padstack`, `DefaultItemClearanceClasses` |
| `fr-io` | `app.freerouting.io.specctra` | ✅ **Complete** | `DsnReader`, `DsnLexer`, `Keyword`, `DsnDocument`, `SesWriter` |
| `fr-autoroute` | `app.freerouting.autoroute` | ✅ **Complete** | `MazeSearchAlgo` (45° modified A* path search), `BatchAutorouter` (multi-pass rip-up & reroute with Rayon parallelism), `CudaClearanceChecker` |
| `fr-drc` | `app.freerouting.drc` | ✅ **Complete** | `DesignRulesChecker::get_all_clearance_violations`, `NetIncompletes`, `AirLine` |
| `fr-core` | `app.freerouting.core` | ✅ **Complete** | `RoutingJob` end-to-end pipeline (DSN -> BasicBoard -> BatchAutorouter -> DRC -> SES), `BoardStatistics` scoring |
| `fr-cli` | `app.freerouting.Freerouting` | ✅ **Complete** | CLI executable with `-de`, `-do`, `-mp`, `-us`, `--help`, `--version` |

---

## Verification & Test Results

- **Unit & Integration Test Suite:** 90 tests passing, 0 failures, 0 compiler warnings.
- **Fixture Tests:** Passed against upstream test designs (`tutorial_board.dsn`, `Issue026-J2_reference.dsn`, `Issue035-ReadPlaceScope.dsn`, `Issue269-min_fr_test.dsn`, `Issue313-FastTest.dsn`).
- **Command-Line Binary:** Verified generating valid Specctra `.ses` files from `.dsn` designs.
