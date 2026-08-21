//! Design rules representation for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.rules` from the upstream Java project:
//!
//! - [`ClearanceMatrix`] — the NxN clearance matrix between item clearance classes.
//! - [`DefaultItemClearanceClasses`] — default clearance class assignments for item types.
//! - [`NetClass`] and [`NetClasses`] — routing rules for individual nets.
//! - [`Net`] and [`Nets`] — electrical nets and subnets.
//! - [`ViaInfo`] and [`ViaInfos`] — via padstack definitions and clearance classes.
//! - [`ViaRule`] — ordered list of vias available for routing a net class.
//! - [`BoardRules`] — master design rules container aggregating all board-level constraints.

pub mod board_rules;
pub mod clearance_matrix;
pub mod default_item_clearance_classes;
pub mod layer;
pub mod net;
pub mod net_class;
pub mod net_classes;
pub mod nets;
pub mod padstack;
pub mod via_info;
pub mod via_infos;
pub mod via_rule;

pub use board_rules::BoardRules;
pub use clearance_matrix::{
    ClearanceMatrix, ClearanceMatrixEntry, ClearanceRow, CLEARANCE_SAFETY_MARGIN,
};
pub use default_item_clearance_classes::{DefaultItemClearanceClasses, ItemClass};
pub use layer::{AngleRestriction, Layer, LayerStructure};
pub use net::Net;
pub use net_class::NetClass;
pub use net_classes::NetClasses;
pub use nets::{Nets, HIDDEN_NET_NO, MAX_LEGAL_NET_NO};
pub use padstack::{PadShape, Padstack};
pub use via_info::ViaInfo;
pub use via_infos::ViaInfos;
pub use via_rule::ViaRule;

/// Returns the crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
