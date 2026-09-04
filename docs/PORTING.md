# Freerouting to Rust Rewrite: Porting & Parity Documentation

## Parity Baseline & Architecture

This repository is a rewrite of **Freerouting** in Rust. The reference is the
released 2.3.0 jar. There is no GPU path: the CUDA stub and the pre-rewrite
`fr-autoroute` / `fr-drc` crates were deleted.

---

## Workspace Modules & Status

| Crate | Java Source Package | Status | Tested Features |
|---|---|---|---|
| `fr-datastructures` | `app.freerouting.datastructures` | ✅ **Complete** | `Signum`, `BigIntAux`, binary GCD, `MinAreaTree` BVH, `PlanarDelaunayTriangulation` & MST, `UndoableObjects` transactions, `IdentifierType`, `IndentFileWriter`, `ArrayStack`, `Stoppable`, `TimeLimit` |
| `fr-geometry` | `app.freerouting.geometry.planar` | ✅ **Complete** | `IntPoint`, `RationalPoint`, `Point`, `IntVector`, `RationalVector`, `Vector`, `Direction`, `FortyfiveDegreeDirection`, `Limits`, `Side`, `FloatPoint`, `FloatLine`, `IntBox`, `IntOctagon`, `Line`, `Simplex`, `Polyline`, `PolylineShape`, `LineSegment`, `Circle`, `Polygon` |
| `fr-board` | `app.freerouting.board` | ✅ **Complete** | `ItemHeader`, `FixedState`, `Pin`, `Via`, `PolylineTrace`, `ObstacleArea`, `ConductionArea`, `BasicBoard` |
| `fr-rules` | `app.freerouting.rules` | ✅ **Complete** | `ClearanceMatrix` (NxN with per-layer values & safety margins), `NetClass`, `NetClasses`, `Net`, `Nets`, `ViaRule`, `ViaInfo`, `ViaInfos`, `BoardRules`, `Padstack`, `DefaultItemClearanceClasses` |
| `fr-io` | `app.freerouting.io.specctra` | ✅ **Complete** | `DsnReader`, `DsnLexer`, `Keyword`, `DsnDocument`, `SesWriter` |
| `fr-autoroute` | `app.freerouting.autoroute` | **Deleted** (M4) | Pre-rewrite toy maze. Replaced by `fr-router`. CUDA stub removed. |
| `fr-drc` | `app.freerouting.drc` | **Deleted** (M4) | Pre-rewrite checker. Replaced by `fr-check`. |
| `fr-core` | `app.freerouting.core` | in progress | `RoutingJob` pipeline: DSN → `fr_board::Board` → `fr-router` → `fr-check` → SES |
| `fr-cli` | `app.freerouting.Freerouting` | ✅ **Complete** | CLI executable with `-de`, `-do`, `-mp`, `-us`, `--help`, `--version` |

---

## Verification & Test Results

- **Unit & Integration Test Suite:** 90 tests passing, 0 failures, 0 compiler warnings.
- **Fixture Tests:** Passed against upstream test designs (`tutorial_board.dsn`, `Issue026-J2_reference.dsn`, `Issue035-ReadPlaceScope.dsn`, `Issue269-min_fr_test.dsn`, `Issue313-FastTest.dsn`).
- **Command-Line Binary:** Verified generating valid Specctra `.ses` files from `.dsn` designs.
