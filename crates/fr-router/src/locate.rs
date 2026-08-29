//! `LocateFoundConnectionAlgo` — the backtrack chain turned into corner lists.
//!
//! Transcribed from
//! `/tmp/fr-java-release/src/main/java/app/freerouting/autoroute/LocateFoundConnectionAlgo.java`
//! (the flat v19-named class the 2.3.0 release actually executes — the
//! conformance appendix §1 of `pull-tight-insert.md`). This module is the
//! abstract base: the constructor loop (`:100-163`, spec §2.3),
//! `calculate_next_trace` (`:347-418`), `adjust_start_corner` (`:444-458`) and
//! the angle-restricted corner helper (`:274-342`, spec §2.4). The per-mode
//! corner emitter lives in [`crate::locate45`].
//!
//! # What this stage is
//!
//! Pure geometry. It reads the room database — never the board, never a mutable
//! anything — and produces [`LocatedConnection`], a self-contained value:
//! per-layer [`ResultItem`] corner lists in **target → start** order, the two
//! end items and their layers, and the ripup claims copied through from the
//! maze result. That self-containment is what lets the caller drop the
//! [`AutorouteEngine`] (which borrows `&Board`) before
//! [`crate::insert`] takes the `&mut Board`. Upstream gets the same effect by
//! calling `AutorouteEngine.clear()` between locate and insert
//! (`AutorouteEngine.java:186`).
//!
//! `backtrack()` itself (spec §2.2) is **not** here: the maze search already
//! materializes it as [`FoundConnection::backtrack`], including the ripped-item
//! collection, because in this port the per-door search state lives in an arena
//! rather than on the door objects. See [`crate::result`].
//!
//! # M2a scope
//!
//! Only the 45°/90° locator is ported ([`crate::locate45`], spec §3.1) — it is
//! the one the default (`AngleRestriction::FortyFive`) board uses. The
//! any-angle funnel locator (spec §3.2) is M2b; see [`locate`] for what an
//! any-angle board gets in the meantime.

use fr_board::item::{ItemId, ItemKind};
use fr_board::shape::AngleRestriction;
use fr_geometry::planar::{FloatLine, FloatPoint, IntPoint, LineSegment, Simplex};

use crate::ctrl::AutorouteControl;
use crate::engine::AutorouteEngine;
use crate::geom;
use crate::result::{BacktrackKind, FoundConnection, RippedItem};
use crate::rooms::ExpandableRef;

/// One trace of the located connection: a corner list on one layer.
///
/// `LocateFoundConnectionAlgo.ResultItem` (`:463-472`). Corners are rounded to
/// integers and consecutive duplicates are removed (`:386-394`), so a
/// `ResultItem` may legitimately hold a **single** corner — the degenerate
/// "already at the target" case the inserter special-cases (spec §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultItem {
    /// The corners, in walking order (target end first).
    pub corners: Vec<IntPoint>,
    /// The layer this trace runs on.
    pub layer: i32,
}

/// The output of the locate stage — everything [`crate::insert`] needs.
#[derive(Debug, Clone)]
pub struct LocatedConnection {
    /// The traces to insert, **target → start**. Consecutive items are joined
    /// by a via at `corners[0]` of the later item (spec §4.1).
    pub connection_items: Vec<ResultItem>,
    /// The item at the start door (the *end* of the backtrack chain).
    pub start_item: ItemId,
    /// The layer of the connection to [`start_item`](Self::start_item).
    pub start_layer: i32,
    /// The item at the maze destination; `None` for a fanout drill destination
    /// (`LocateFoundConnectionAlgo.java:107-112`).
    pub target_item: Option<ItemId>,
    /// The layer of the connection to [`target_item`](Self::target_item).
    pub target_layer: i32,
    /// The ripup claims the found path made, copied from the maze result. The
    /// inserter deletes these (and their connections) in the same transaction.
    pub ripped_items: Vec<RippedItem>,
}

/// Why [`locate`] refused a found connection.
///
/// Each variant is an upstream `FRLogger.warn` plus a null skeleton, which
/// `AutorouteEngine` reports as NOT_ROUTED (spec §9, first bullet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocateFailure {
    /// The chain was empty. Upstream would index `[-1]`.
    EmptyBacktrack,
    /// The last chain element is not a `TargetItemExpansionDoor`
    /// (`LocateFoundConnectionAlgo.java:87-95`).
    StartDoorNotATargetDoor,
    /// The destination door is neither a target door nor a drill (`:113-118`).
    UnexpectedDestinationDoor,
    /// A chain element carries no room, so no corner can be placed in it
    /// (`LocateFoundConnectionAlgo45Degree.java:106-109`).
    MissingRoom,
}

/// `LocateFoundConnectionAlgo.get_instance` (`:168-180`) — dispatch on the
/// board's angle restriction, then run the constructor loop.
///
/// # Angle modes
///
/// | board mode | upstream | M2a |
/// |---|---|---|
/// | `Ninety` | `LocateFoundConnectionAlgo45Degree` | same, with 90° knees |
/// | `FortyFive` | `LocateFoundConnectionAlgo45Degree` | same |
/// | `None` | `LocateFoundConnectionAlgoAnyAngle` | **the 45° locator** |
///
/// The last row is the M2b seam. Routing an any-angle board with the 45°
/// locator is *legal* — every 45° polyline is an any-angle polyline, and the
/// corner placement still lands inside the shrunken rooms — but it is not the
/// release's geometry: the funnel locator produces taut visibility paths, this
/// produces axis+diagonal staircases through every door. The consequence is
/// longer traces on any-angle boards, never a clearance violation. Replacing it
/// is `locate_any_angle`, spec §3.2.
pub fn locate(
    engine: &AutorouteEngine<'_>,
    connection: &FoundConnection,
) -> Result<LocatedConnection, LocateFailure> {
    let hops = collect_hops(engine, connection)?;
    let angle_restriction = engine.board.angle_restriction();
    let mut located = Locator::new(&hops, &engine.ctrl, angle_restriction)?.run()?;
    // The ripup claims travel with the located connection: the inserter deletes
    // them in the same transaction as the new copper
    // (`AutorouteEngine.java:221-239`).
    located.ripped_items.clone_from(&connection.ripped_items);
    if crate::rooms::trace::is_enabled() {
        crate::rooms::trace::located(
            engine.ctrl.net_no,
            hops.len(),
            located
                .connection_items
                .iter()
                .map(|item| item.corners.len())
                .sum(),
            located.connection_items.len(),
        );
    }
    Ok(located)
}

/// One backtrack hop, with everything the corner placement reads pulled out of
/// the arenas.
///
/// The locator is a pure function of this slice; nothing below it touches the
/// engine again. `BacktrackElement` already carries most of it, and the two
/// fields it does not (the door's dimension and its section line) are read here
/// once instead of per corner.
#[derive(Debug, Clone)]
pub(crate) struct Hop {
    /// What object this hop crosses.
    pub kind: BacktrackKind,
    /// Which section of it.
    pub section_no: u16,
    /// `door.get_shape()`.
    pub shape: Simplex,
    /// `door.get_dimension()`: 1 for a line door, 2 for an area door, a target
    /// door (`TargetItemExpansionDoor.java:41`) and a drill
    /// (`ExpansionDrill.java:101`).
    pub dimension: i32,
    /// The segment `get_section_segments` divides, for a 1-dimensional door.
    pub door_line: Option<FloatLine>,
    /// The room this hop shares with the **next** hop.
    pub room_shape: Option<Simplex>,
    /// That room's layer.
    pub room_layer: Option<i32>,
    /// Whether that room is an `ObstacleExpansionRoom` — it shrinks by the bare
    /// half width, a free room by half width + `TRACE_WIDTH_TOLERANCE`
    /// (`LocateFoundConnectionAlgo45Degree.java:116-121`).
    pub room_is_obstacle: bool,
    /// For a target door: `item.get_trace_connection_shape(tree, tree_entry_no)
    /// ∩ room.shape`, the shape the route must land in
    /// (`LocateFoundConnectionAlgo.java:146-147, :186-188`).
    pub connection_shape: Option<Simplex>,
}

/// `Connectable.get_trace_connection_shape(tree, index)` — **not** the item's
/// tree shape.
///
/// The distinction is load-bearing for connectivity, and easy to miss: a
/// `DrillItem` (pin or via) answers `TileShape.get_instance(get_center())`, a
/// *point* at its centre (`DrillItem.java:350-352`), so the route lands exactly
/// on the centre and the exact-point contact predicate
/// (`drc-connectivity.md` §4.3) sees a connection. Landing anywhere else inside
/// the pad would leave the net unrouted as far as the referee is concerned.
/// A trace answers the bare segment (`PolylineTrace.java:918-927`) and a
/// conduction area its tree shape (`ConductionArea.java:373-379`).
///
/// [`AutorouteEngine::trace_connection_shape`] returns the compensated tile
/// shape for every class, which is the right answer for the maze's target-door
/// *reachability* question and the wrong one here.
fn trace_connection_shape(
    engine: &AutorouteEngine<'_>,
    item: ItemId,
    tree_entry_no: u16,
) -> Option<Simplex> {
    let board_item = engine.board.store().get(item)?;
    match &board_item.kind {
        ItemKind::Pin(pin) => Some(pin.center.surrounding_box().to_simplex()),
        ItemKind::Via(via) => Some(via.center.surrounding_box().to_simplex()),
        ItemKind::Trace(trace) => {
            LineSegment::from_polyline(&trace.polyline, tree_entry_no as usize + 1)
                .map(|segment| segment.to_simplex())
        }
        _ => engine.trace_connection_shape(item, tree_entry_no),
    }
}

fn collect_hops(
    engine: &AutorouteEngine<'_>,
    connection: &FoundConnection,
) -> Result<Vec<Hop>, LocateFailure> {
    if connection.backtrack.is_empty() {
        return Err(LocateFailure::EmptyBacktrack);
    }
    let mut hops = Vec::with_capacity(connection.backtrack.len());
    for element in &connection.backtrack {
        let (dimension, door_line) = match element.door {
            ExpandableRef::Door(door_id) => {
                let door = engine.rooms.door(door_id);
                (door.dimension, door.door_line)
            }
            // Both report dimension 2 and are never sectioned by the locator.
            ExpandableRef::Drill(_) | ExpandableRef::Page(_) => (2, None),
        };
        // `:146-147` / `:186-188`: the landing shape of a target door is the
        // item's *connection* shape clipped to the room, which is not the
        // door's cached shape — see [`trace_connection_shape`].
        let connection_shape = match element.kind {
            BacktrackKind::TargetDoor {
                item,
                tree_entry_no,
            } => trace_connection_shape(engine, item, tree_entry_no).map(|shape| {
                match element.next_room_shape.as_ref() {
                    Some(room) => shape.intersection(room),
                    None => shape,
                }
            }),
            _ => None,
        };
        hops.push(Hop {
            kind: element.kind,
            section_no: element.section_no_of_door,
            shape: element.door_shape.clone(),
            dimension,
            door_line,
            room_shape: element.next_room_shape.clone(),
            room_layer: element.next_room_layer,
            room_is_obstacle: element.next_room_is_obstacle,
            connection_shape,
        });
    }
    Ok(hops)
}

/// The cursor state of `LocateFoundConnectionAlgo` while it walks the chain.
pub(crate) struct Locator<'c> {
    pub(crate) hops: &'c [Hop],
    pub(crate) ctrl: &'c AutorouteControl,
    pub(crate) angle_restriction: AngleRestriction,
    /// `start_door.item` / its room's layer.
    start_item: ItemId,
    start_layer: i32,
    /// The shape the last trace must land in: the start item's connection shape
    /// clipped to the start room (`:146-156`).
    start_target_shape: Simplex,
    target_item: Option<ItemId>,
    target_layer: i32,

    pub(crate) current_from_point: FloatPoint,
    pub(crate) previous_from_point: FloatPoint,
    pub(crate) current_trace_layer: i32,
    pub(crate) current_from_door_index: usize,
    pub(crate) current_to_door_index: usize,
    pub(crate) current_target_door_index: usize,
    pub(crate) current_target_shape: Simplex,
    at_fanout_end: bool,

    /// Reusable buffers: one corner list and one section-segment list for the
    /// whole walk (ARCHITECTURE — no allocation in the corner loop).
    pub(crate) scratch_corners: Vec<FloatPoint>,
    pub(crate) scratch_sections: Vec<FloatLine>,
}

impl<'c> Locator<'c> {
    /// `LocateFoundConnectionAlgo`'s constructor up to the main loop
    /// (`:60-121`).
    fn new(
        hops: &'c [Hop],
        ctrl: &'c AutorouteControl,
        angle_restriction: AngleRestriction,
    ) -> Result<Locator<'c>, LocateFailure> {
        let start_hop = hops.last().ok_or(LocateFailure::EmptyBacktrack)?;
        let BacktrackKind::TargetDoor {
            item: start_item, ..
        } = start_hop.kind
        else {
            // `:87-95`: warn, null skeleton, empty connection.
            return Err(LocateFailure::StartDoorNotATargetDoor);
        };
        let start_layer = start_hop.room_layer.ok_or(LocateFailure::MissingRoom)?;

        // `:146-156` — the shape of the final target, precomputed because the
        // main loop needs it on its last iteration only.
        let mut start_target_shape = start_hop
            .connection_shape
            .clone()
            .ok_or(LocateFailure::StartDoorNotATargetDoor)?;
        if start_target_shape.dimension() >= 2 {
            // A conduction-area target: pull the landing point inside by the
            // compensated half width so the connection is safe (spec §9).
            let shrunk = start_target_shape.offset(-ctrl.compensated_half_width(start_layer));
            if !shrunk.is_empty() {
                start_target_shape = shrunk;
            }
        }
        let first = &hops[0];
        let (target_item, target_layer, current_from_point, at_fanout_end) = match first.kind {
            BacktrackKind::TargetDoor { item, .. } => {
                let layer = first.room_layer.ok_or(LocateFailure::MissingRoom)?;
                // `calculate_starting_point` (`:185-189`): the centre of
                // gravity of `connection_shape ∩ room`, rounded.
                let connection_shape = first
                    .connection_shape
                    .as_ref()
                    .ok_or(LocateFailure::UnexpectedDestinationDoor)?;
                let start_point = connection_shape.centre_of_gravity().round().to_float();
                (Some(item), layer, start_point, false)
            }
            BacktrackKind::Drill {
                location,
                first_layer,
                ..
            } => {
                // Fanout: the route ends *at* the drill (`:107-112`).
                let layer = first_layer + first.section_no as i32;
                (None, layer, location.to_float(), true)
            }
            BacktrackKind::RoomDoor => return Err(LocateFailure::UnexpectedDestinationDoor),
        };

        Ok(Locator {
            hops,
            ctrl,
            angle_restriction,
            start_item,
            start_layer,
            start_target_shape,
            target_item,
            target_layer,
            current_from_point,
            previous_from_point: current_from_point,
            current_trace_layer: target_layer,
            current_from_door_index: 0,
            current_to_door_index: 0,
            current_target_door_index: 0,
            current_target_shape: Simplex::EMPTY,
            at_fanout_end,
            scratch_corners: Vec::new(),
            scratch_sections: Vec::new(),
        })
    }

    /// The constructor's main loop (`:122-162`, spec §2.3).
    fn run(mut self) -> Result<LocatedConnection, LocateFailure> {
        let mut connection_items = Vec::new();
        let mut connection_done = false;
        // The chain length bounds the number of traces: every iteration either
        // finishes the connection or consumes at least one drill hop.
        let mut guard = self.hops.len() + 1;

        while !connection_done {
            if guard == 0 {
                // Unreachable while `current_from_door_index` advances past a
                // drill each round; a truncated connection beats a hang.
                break;
            }
            guard -= 1;

            let mut layer_changed = false;
            if self.at_fanout_end {
                // `:125-127`: do *not* advance `current_target_door_index`.
                layer_changed = true;
            } else {
                self.current_target_door_index = self.current_from_door_index + 1;
                while self.current_target_door_index < self.hops.len() && !layer_changed {
                    if matches!(
                        self.hops[self.current_target_door_index].kind,
                        BacktrackKind::Drill { .. }
                    ) {
                        layer_changed = true;
                    } else {
                        self.current_target_door_index += 1;
                    }
                }
            }

            if layer_changed {
                // The next trace leads to a via: aim at the drill point.
                let BacktrackKind::Drill { location, .. } =
                    self.hops[self.current_target_door_index].kind
                else {
                    return Err(LocateFailure::UnexpectedDestinationDoor);
                };
                // `TileShape.get_instance(Point)` is the point's surrounding
                // box (`TileShape.java:69-71`).
                self.current_target_shape = location.surrounding_box().to_simplex();
            } else {
                connection_done = true;
                self.current_target_door_index = self.hops.len() - 1;
                self.current_target_shape = self.start_target_shape.clone();
            }

            self.current_to_door_index = self.current_from_door_index + 1;
            let at_fanout_end = self.at_fanout_end;
            self.at_fanout_end = false;
            connection_items.push(self.calculate_next_trace(layer_changed, at_fanout_end)?);
        }

        Ok(LocatedConnection {
            connection_items,
            start_item: self.start_item,
            start_layer: self.start_layer,
            target_item: self.target_item,
            target_layer: self.target_layer,
            ripped_items: Vec::new(),
        })
    }

    /// `calculate_next_trace` (`:347-418`).
    fn calculate_next_trace(
        &mut self,
        layer_changed: bool,
        at_fanout_end: bool,
    ) -> Result<ResultItem, LocateFailure> {
        let mut corner_list: Vec<FloatPoint> = Vec::new();
        corner_list.push(self.current_from_point);

        if !at_fanout_end {
            let adjusted = self.adjust_start_corner();
            if adjusted != self.current_from_point {
                let add_corner = calculate_additional_corner(
                    self.current_from_point,
                    adjusted,
                    true,
                    self.angle_restriction,
                );
                corner_list.push(add_corner);
                corner_list.push(adjusted);
                self.previous_from_point = self.current_from_point;
                self.current_from_point = adjusted;
            }
        }

        // `:360-374`. Upstream's `curr_next_corner != prev_corner` is a
        // reference comparison; value equality is used here. The two differ
        // only when the emitter returns two *distinct* objects with identical
        // coordinates, in which case upstream pushes both and the rounding
        // dedupe below drops one anyway — and `previous_from_point`, the only
        // other state involved, is read exclusively by the any-angle locator.
        let mut prev_corner = self.current_from_point;
        loop {
            let mut next_corners = std::mem::take(&mut self.scratch_corners);
            self.calculate_next_trace_corners(&mut next_corners)?;
            if next_corners.is_empty() {
                self.scratch_corners = next_corners;
                break;
            }
            for curr in next_corners.iter().copied() {
                if curr != prev_corner {
                    corner_list.push(curr);
                    self.previous_from_point = self.current_from_point;
                    self.current_from_point = curr;
                    prev_corner = curr;
                }
            }
            self.scratch_corners = next_corners;
        }

        let mut next_layer = self.current_trace_layer;
        if layer_changed {
            self.current_from_door_index = self.current_target_door_index + 1;
            // `:379-382`: a chain that ends in a drill has no room past it.
            if let Some(hop) = self.hops.get(self.current_from_door_index) {
                if let Some(layer) = hop.room_layer {
                    next_layer = layer;
                }
            }
        }

        // `:386-394`: round to integers, dropping consecutive duplicates.
        let mut corners: Vec<IntPoint> = Vec::with_capacity(corner_list.len());
        for corner in &corner_list {
            let rounded = corner.round();
            if corners.last() != Some(&rounded) {
                corners.push(rounded);
            }
        }

        let item = ResultItem {
            corners,
            layer: self.current_trace_layer,
        };
        self.current_trace_layer = next_layer;
        Ok(item)
    }

    /// `adjust_start_corner` (`:444-458`): pull the from-point into the start
    /// room shrunk by the compensated half width, so the trace leaving it stays
    /// inside.
    fn adjust_start_corner(&self) -> FloatPoint {
        let Some(hop) = self.hops.get(self.current_from_door_index) else {
            return self.current_from_point;
        };
        let Some(room_shape) = hop.room_shape.as_ref() else {
            return self.current_from_point;
        };
        let trace_half_width = self.ctrl.compensated_half_width(self.current_trace_layer);
        let shrunk = room_shape.offset(-trace_half_width);
        if shrunk.is_empty() || shrunk.contains_float(&self.current_from_point, 0.0) {
            return self.current_from_point;
        }
        shrunk
            .nearest_point_approx(&self.current_from_point)
            .round()
            .to_float()
    }

    /// The per-mode corner emitter. M2a always dispatches to the 45° one; see
    /// [`locate`] for why.
    fn calculate_next_trace_corners(
        &mut self,
        out: &mut Vec<FloatPoint>,
    ) -> Result<(), LocateFailure> {
        crate::locate45::calculate_next_trace_corners_45(self, out)
    }

    /// The section segments of a 1-dimensional door, for `section_no_of_door`.
    ///
    /// `ExpansionDoor.get_section_segments(offset)` (`:113-148`), restricted to
    /// the `dimension == 1` branch — the only one the 45° locator reaches
    /// (`LocateFoundConnectionAlgo45Degree.java:188`). It duplicates
    /// [`crate::rooms::RoomArena::section_segments`] deliberately: that one
    /// needs `&mut` access to the node arena to (re)allocate the door's search
    /// state, which the locator has no business touching, and it takes the same
    /// two-line path for this case.
    pub(crate) fn section_segments(&mut self, hop_index: usize, trace_half_width: f64) {
        self.scratch_sections.clear();
        let Some(hop) = self.hops.get(hop_index) else {
            return;
        };
        let Some(line) = hop.door_line else {
            return;
        };
        // `:114` — the tolerance is added inside `get_section_segments`.
        let offset = trace_half_width + crate::ctrl::TRACE_WIDTH_TOLERANCE as f64;
        let shrunk = geom::shrink_segment(&line, offset);
        // `:144-145` — max section width is `10 × offset`, count derived from
        // the *unshrunk* segment.
        let count = (line.b.distance(&line.a) / (10.0 * offset)) as usize + 1;
        geom::divide_segment_into_sections(&shrunk, count, &mut self.scratch_sections);
    }
}

/// `calculate_additional_corner` (`:332-342`, spec §2.4).
///
/// The knee that makes the two segments `from → result` and `result → to`
/// satisfy the angle restriction. Shared with the neckdown splitter
/// (`InsertFoundConnectionAlgo.java:389, :402`), which is why it is public.
pub fn calculate_additional_corner(
    from: FloatPoint,
    to: FloatPoint,
    horizontal_first: bool,
    angle_restriction: AngleRestriction,
) -> FloatPoint {
    match angle_restriction {
        AngleRestriction::Ninety => ninety_degree_corner(from, to, horizontal_first),
        AngleRestriction::FortyFive => fortyfive_degree_corner(from, to, horizontal_first),
        // `:338-340`: no extra corner in free-angle mode.
        AngleRestriction::None => to,
    }
}

/// `ninety_degree_corner` (`:274-285`).
fn ninety_degree_corner(from: FloatPoint, to: FloatPoint, horizontal_first: bool) -> FloatPoint {
    if horizontal_first {
        FloatPoint::new(to.x, from.y)
    } else {
        FloatPoint::new(from.x, to.y)
    }
}

/// `fortyfive_degree_corner` (`:287-327`).
fn fortyfive_degree_corner(from: FloatPoint, to: FloatPoint, horizontal_first: bool) -> FloatPoint {
    let abs_dx = (to.x - from.x).abs();
    let abs_dy = (to.y - from.y).abs();
    if abs_dx <= abs_dy {
        if horizontal_first {
            let y = if to.y >= from.y {
                from.y + abs_dx
            } else {
                from.y - abs_dx
            };
            FloatPoint::new(to.x, y)
        } else {
            let y = if to.y > from.y {
                to.y - abs_dx
            } else {
                to.y + abs_dx
            };
            FloatPoint::new(from.x, y)
        }
    } else if horizontal_first {
        let x = if to.x > from.x {
            to.x - abs_dy
        } else {
            to.x + abs_dy
        };
        FloatPoint::new(x, from.y)
    } else {
        let x = if to.x > from.x {
            from.x + abs_dy
        } else {
            from.x - abs_dy
        };
        FloatPoint::new(x, to.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> FloatPoint {
        FloatPoint::new(x, y)
    }

    #[test]
    fn ninety_degree_knee_is_the_l_corner() {
        let from = p(0.0, 0.0);
        let to = p(100.0, 40.0);
        assert_eq!(
            calculate_additional_corner(from, to, true, AngleRestriction::Ninety),
            p(100.0, 0.0)
        );
        assert_eq!(
            calculate_additional_corner(from, to, false, AngleRestriction::Ninety),
            p(0.0, 40.0)
        );
    }

    #[test]
    fn fortyfive_degree_knee_makes_both_legs_legal() {
        // A mostly-horizontal move: |dx| > |dy|.
        let from = p(0.0, 0.0);
        let to = p(100.0, 40.0);
        let knee = calculate_additional_corner(from, to, true, AngleRestriction::FortyFive);
        // horizontal first: the first leg runs along y = from.y, the second is
        // the 45° diagonal.
        assert_eq!(knee, p(60.0, 0.0));
        assert!((knee.x - to.x).abs() == (knee.y - to.y).abs());
        let knee = calculate_additional_corner(from, to, false, AngleRestriction::FortyFive);
        // diagonal first, then horizontal along y = to.y.
        assert_eq!(knee, p(40.0, 40.0));
        assert!((knee.x - from.x).abs() == (knee.y - from.y).abs());
    }

    #[test]
    fn fortyfive_degree_knee_on_a_mostly_vertical_move() {
        let from = p(0.0, 0.0);
        let to = p(40.0, 100.0);
        let knee = calculate_additional_corner(from, to, true, AngleRestriction::FortyFive);
        assert_eq!(knee, p(40.0, 40.0));
        let knee = calculate_additional_corner(from, to, false, AngleRestriction::FortyFive);
        assert_eq!(knee, p(0.0, 60.0));
    }

    #[test]
    fn free_angle_needs_no_extra_corner() {
        let from = p(0.0, 0.0);
        let to = p(100.0, 40.0);
        assert_eq!(
            calculate_additional_corner(from, to, true, AngleRestriction::None),
            to
        );
    }
}
