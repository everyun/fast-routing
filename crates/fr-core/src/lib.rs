//! Headless board orchestration and scoring for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.core` from the upstream Java project:
//!
//! - `RoutingJob` — complete pipeline runner (DSN -> BasicBoard -> BatchAutorouter -> DRC -> SES).
//! - `BoardStatistics` — routing completion and objective cost scoring.

pub mod board_statistics;
pub mod routing_job;

pub use board_statistics::BoardStatistics;
pub use routing_job::{JobResult, RoutingJob};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
