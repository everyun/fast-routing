//! Core PCB autorouting engine for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.autoroute` from the upstream Java project:
//!
//! - `MazeSearchAlgo` — 45-degree obstacle-avoiding maze path search (modified A*).
//! - `BatchAutorouter` — iterative multi-pass rip-up & reroute loop with Rayon parallelism.
//! - `LayerSpatialGrid` — high-performance O(1) spatial hashing obstacle query grid.
//! - `CudaClearanceChecker` — GPU / SIMD accelerated batch clearance queries.
//! - `RoutingStatistics` — convergence and routing completion statistics.

pub mod batch_autorouter;
pub mod cuda_accel;
pub mod maze_search;
pub mod net_connectivity;
pub mod spatial_grid;

pub use batch_autorouter::{BatchAutorouter, BatchRouterSettings, RoutingStatistics};
pub use cuda_accel::{CudaClearanceChecker, CudaConfig};
pub use maze_search::{MazeSearchAlgo, MazeSearchSettings, RoutePath, RoutePath3D, RouteSegment3D, RouteVia3D};
pub use net_connectivity::{analyze_net_connectivity, NetConnectivityStatus};
pub use spatial_grid::LayerSpatialGrid;
