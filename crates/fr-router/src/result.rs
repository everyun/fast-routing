//! The found-connection result: the backtrack chain the locator consumes.
//!
//! `pull-tight-insert.md` §2.1-2.2 fixes the contract.
//! `LocateFoundConnectionAlgo` walks `MazeSearchElement.backtrack_door` from
//! the destination to the start door and materializes a
//! `BacktrackElement { door, section_no_of_door, next_room }` per hop, where
//! `next_room` is *the room common to this door and the next door in the list*.
//! It also collects the ripped items as it goes.
//!
//! That walk is done here, once, at the end of the search, so the locator gets
//! a plain `Vec` and never has to touch the node arena. Everything it reads is
//! carried along: the shape and layer of the door, and the room's shape and
//! layer (`pull-tight-insert.md` §3 needs the room shape to shrink by the
//! compensated half width).

use fr_board::item::ItemId;
use fr_geometry::planar::{IntPoint, Simplex};

use crate::maze::nodes::Adjustment;
use crate::rooms::{ExpandableRef, RoomId};

/// What kind of object one backtrack hop crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacktrackKind {
    /// An `ExpansionDoor` between two rooms.
    RoomDoor,
    /// A `TargetItemExpansionDoor`; the item and tile index are carried.
    TargetDoor {
        /// The start or destination item.
        item: ItemId,
        /// Which of its tile shapes.
        tree_entry_no: u16,
    },
    /// An `ExpansionDrill`; `section_no_of_door` is `layer - first_layer`.
    Drill {
        /// The drill point.
        location: IntPoint,
        /// First layer of the drill span.
        first_layer: i32,
        /// Last layer of the drill span.
        last_layer: i32,
    },
}

/// One hop of the backtrack chain, destination first.
#[derive(Debug, Clone)]
pub struct BacktrackElement {
    /// The object crossed.
    pub door: ExpandableRef,
    /// Which section of it.
    pub section_no_of_door: u16,
    /// What kind of object it is, with the payload the locator needs.
    pub kind: BacktrackKind,
    /// The door's shape (upstream's `door.get_shape()`), cached at creation.
    pub door_shape: Simplex,
    /// The room common to this door and the **next** element of the chain.
    /// `None` only when the chain ends in a drill with no room on that layer.
    pub next_room: Option<RoomId>,
    /// That room's shape, so the locator does not need the arena.
    pub next_room_shape: Option<Simplex>,
    /// That room's layer.
    pub next_room_layer: Option<i32>,
    /// Whether that room is an obstacle room, i.e. a planned ripup.
    pub next_room_is_obstacle: bool,
    /// The settled `room_ripped` flag of this hop.
    pub room_ripped: bool,
    /// The settled toll of this hop.
    pub ripup_cost: i32,
    /// The settled shove adjustment of this hop. Always
    /// [`Adjustment::None`](crate::maze::nodes::Adjustment::None) in M2a.
    pub adjustment: Adjustment,
}

/// One item the found path plans to rip up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RippedItem {
    /// The item to delete before the new connection is inserted.
    pub item: ItemId,
    /// The toll the search paid for it.
    pub ripup_cost: i32,
}

/// The result of a successful [`MazeSearch`](crate::maze::search::MazeSearch).
///
/// `MazeSearchAlgo.Result` is only `(destination_door, section_no)`; everything
/// else the locator needs is reachable from the door in Java because the state
/// lives on the door objects. Here the chain is materialized.
#[derive(Debug, Clone)]
pub struct FoundConnection {
    /// The destination object and section, upstream's `Result`.
    pub destination_door: ExpandableRef,
    /// Section index of the destination object.
    pub section_no_of_destination_door: u16,
    /// The backtrack chain, **destination first, start door last**.
    ///
    /// `pull-tight-insert.md` §2.1: the last element's door must be a
    /// [`BacktrackKind::TargetDoor`], or the locator refuses the connection.
    pub backtrack: Vec<BacktrackElement>,
    /// The items the path plans to rip up, in the order the walk found them.
    pub ripped_items: Vec<RippedItem>,
}

impl FoundConnection {
    /// The start item: the item of the last chain element, which must be a
    /// target door.
    pub fn start_item(&self) -> Option<(ItemId, u16)> {
        match self.backtrack.last()?.kind {
            BacktrackKind::TargetDoor {
                item,
                tree_entry_no,
            } => Some((item, tree_entry_no)),
            _ => None,
        }
    }

    /// The destination item, or `None` for a fanout drill destination.
    pub fn target_item(&self) -> Option<(ItemId, u16)> {
        match self.backtrack.first()?.kind {
            BacktrackKind::TargetDoor {
                item,
                tree_entry_no,
            } => Some((item, tree_entry_no)),
            _ => None,
        }
    }

    /// The layer the route starts on.
    pub fn start_layer(&self) -> Option<i32> {
        self.backtrack.last()?.next_room_layer
    }

    /// The layer the route ends on.
    pub fn target_layer(&self) -> Option<i32> {
        self.backtrack.first()?.next_room_layer
    }

    /// True when the chain satisfies the locator's invariant
    /// (`LocateFoundConnectionAlgo.java:87-95`).
    pub fn is_well_formed(&self) -> bool {
        !self.backtrack.is_empty() && self.start_item().is_some()
    }
}
