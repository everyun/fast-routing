//! The pre-rewrite board model.
//!
//! Its last consumers (`fr-autoroute`, `fr-drc`, `fr-core::legacy`) were deleted
//! in M4. The module remains only because the crate root still re-exports
//! [`BasicBoard`] and its per-type `Vec`s; the real model is one heterogeneous
//! [`ItemStore`](crate::store::ItemStore) with descending-id iteration, not five
//! parallel arrays with `Vec` positions for identity.
//!
//! Everything here is the old code verbatim, with one change: [`ItemHeader`]
//! now carries the new [`FixedState`](crate::item::FixedState), whose variants
//! and names are identical but which additionally has the upstream ordinal
//! [`Ord`] — so downstream `match` arms are unaffected.
//!
//! Nothing in this module is used by the new item store, the journal or the
//! board history. It is deleted together with its last consumer.

pub mod area;
pub mod basic_board;
pub mod pin;
pub mod trace;
pub mod via;

pub use area::{ConductionArea, ObstacleArea};
pub use basic_board::BasicBoard;
pub use pin::Pin;
pub use trace::PolylineTrace;
pub use via::Via;

use crate::item::FixedState;

/// Abstract item classification on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    Pin,
    Via,
    Trace,
    ObstacleArea,
    ConductionArea,
    ComponentOutline,
    BoardOutline,
}

/// Common metadata for every placed item on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemHeader {
    pub id_no: i32,
    pub net_no_arr: Vec<i32>,
    pub clearance_class: i32,
    pub component_no: i32,
    pub fixed_state: FixedState,
    pub on_the_board: bool,
}

impl ItemHeader {
    pub fn new(id_no: i32, net_no_arr: Vec<i32>, clearance_class: i32, component_no: i32) -> Self {
        ItemHeader {
            id_no,
            net_no_arr,
            clearance_class,
            component_no,
            fixed_state: FixedState::Unfixed,
            on_the_board: true,
        }
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed_state != FixedState::Unfixed
    }

    pub fn contains_net(&self, net_no: i32) -> bool {
        self.net_no_arr.contains(&net_no)
    }
}
