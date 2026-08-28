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
//!
//! Two modules do not have a 1:1 Java counterpart:
//!
//! - [`clearance_class_names`] — the clearance-class *name algebra* that
//!   `io.specctra.parser.Structure` and `io.specctra.parser.Network` perform inline while
//!   importing `(clearance … (type …))` rules, extracted as pure functions for the importer.
//! - [`router_settings`] — `app.freerouting.settings.RouterSettings` and its sub-settings,
//!   including the board-geometry-derived per-layer cost preparation.
//!
//! Upstream reference: freerouting 2.3.0 (`Build-Revision: 2d4de019`). Every deliberate
//! deviation is called out in a `Parity note:` doc comment; behavioural quirks are reproduced
//! rather than fixed (see `docs/PARITY-DIVERGENCES.md`).

pub mod board_rules;
pub mod clearance_class_names;
pub mod clearance_matrix;
pub mod default_item_clearance_classes;
pub mod layer;
pub mod net;
pub mod net_class;
pub mod net_classes;
pub mod nets;
pub mod padstack;
pub mod router_settings;
pub mod via_info;
pub mod via_infos;
pub mod via_rule;

pub use board_rules::BoardRules;
pub use clearance_class_names::{
    append_clearance_class, contains_wire_clearance_pair, create_default_clearance_classes,
    default_via_info_name, is_smd_to_turn_gap, is_wire_alias, net_class_item_clearance_class_name,
    network_class_pair, reserved_item_class, structure_class_pair, ClassPairResolution,
    CLASS_CLEARANCE_SEPARATOR, RESERVED_ITEM_CLASS_NAMES, SMD_TO_TURN_GAP, WIRE_CLASS_NAME,
    WIRE_CLEARANCE_CLASS_NO,
};
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
pub use router_settings::{
    default_max_threads, normalize_max_threads, BoardAspectRatio, BoardUpdateStrategy,
    ExpansionCostFactor, FanoutSettings, ItemSelectionStrategy, LayerSettings, OptimizerSettings,
    RouterSettings, ScoringSettings, ALGORITHM_CURRENT, ALGORITHM_V19,
    DEFAULT_TRACE_PULL_TIGHT_ACCURACY, MAX_BEND_COST, MAX_PASSES_LIMIT, MIN_BEND_COST,
};
pub use via_info::ViaInfo;
pub use via_infos::ViaInfos;
pub use via_rule::ViaRule;

/// Returns the crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
