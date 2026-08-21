//! Design Rule Checker (DRC) for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.drc` from the upstream Java project:
//!
//! - `DesignRulesChecker::get_all_clearance_violations` — Authoritative clearance checker.
//! - `ClearanceViolation` — structured clearance violation info.
//! - `NetIncompletes`, `AirLine` — unrouted net / ratsnest analysis.

pub mod clearance_violation;
pub mod drc_checker;
pub mod net_incompletes;

pub use clearance_violation::ClearanceViolation;
pub use drc_checker::DesignRulesChecker;
pub use net_incompletes::{AirLine, NetIncompletes};
