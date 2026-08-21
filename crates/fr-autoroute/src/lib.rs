//! Core PCB autorouting engine for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.autoroute` from the upstream Java project:
//!
//! - `MazeSearchAlgo` — 45-degree obstacle-avoiding maze path search (modified A*).
//! - `BatchAutorouter` — iterative multi-pass rip-up & reroute loop with Rayon parallelism.
//! - `CudaClearanceChecker` — GPU / SIMD accelerated batch clearance queries.
//! - `RoutingStatistics` — convergence and routing completion statistics.

pub mod batch_autorouter;
pub mod cuda_accel;
pub mod maze_search;

pub use batch_autorouter::{BatchAutorouter, BatchRouterSettings, RoutingStatistics};
pub use cuda_accel::{CudaClearanceChecker, CudaConfig};
pub use maze_search::{MazeSearchAlgo, MazeSearchSettings, RoutePath};
