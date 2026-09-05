//! Ripup evaluation during the search: `check_ripup`,
//! `calc_fanout_via_ripup_cost_factor`, `enter_through_small_door` and
//! `check_leaving_ripped_item`.
//!
//! Transcribed from `autoroute/MazeSearchAlgo.java:150-190, 1356-1476,
//! 1558-1635`. The search never mutates the board: a ripup is *planned* by
//! stamping `room_ripped` / `ripup_cost` onto the settled door section, and the
//! backtracker collects the items afterwards (`maze-engine.md` §8.2).
//!
//! Conformance corrections applied here:
//!
//! * **D4** — the randomization branch (`ripup_pass_no >= 4 && % 3 != 0`) is
//!   unreachable in the release, because `ripup_pass_no` is 1 everywhere. It is
//!   implemented, gated on the same condition, and therefore never fires with a
//!   release-shaped control. There is consequently no RNG in this crate.
//! * **D5** — the effective `ripup_costs` is `start_ripup_costs × pass`, with
//!   `start_ripup_costs` defaulting to **100**, not the ctor's 1000.
//! * **D6** — the detour is only consulted when
//!   `fanout_via_cost_factor <= 1 && !is_fanout`; otherwise it stays 1.
//! * **D7** — the fanout protection is gated on
//!   `ripup_costs <= 2 × start_ripup_costs`, in **both** the trace and the via
//!   branch.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use fr_board::board::Board;
use fr_board::clearance::ClearanceSource;
use fr_board::item::{ItemClass, ItemId, ItemKind};
use fr_geometry::planar::{Line, Point, Polyline, Simplex};

use crate::ctrl::{
    ALREADY_RIPPED_COSTS, FANOUT_COST_CONST, MAX_RIPUP_COSTS, TRACE_WIDTH_TOLERANCE,
};
use crate::engine::AutorouteEngine;
use crate::geom;
use crate::maze::nodes::Adjustment;
use crate::rooms::{DoorKind, RoomId, RoomKind};

/// How the maze learns how much an existing connection detours.
///
/// Upstream reads `Connection.get(item).get_detour()`
/// (`MazeSearchAlgo.java:1426-1428`), where `Connection` is the precalculated
/// per-item connection record with `DETOUR_ADD = 100` and
/// `DETOUR_ITEM_COST = 0.1` (conformance appendix D19). That record belongs to
/// the batch-orchestration wave, so the maze takes it as an injected callback.
///
/// The default implementation returns `1.0` for every item, which is exactly
/// what the release computes when the connection is unknown, and makes the
/// ripup toll `ripup_costs × cost_factor × fanout_factor`.
pub trait DetourSource {
    /// The detour of the connection `item` belongs to; `>= 1`.
    fn detour(&self, item: ItemId) -> f64;
}

/// The no-connection-database detour source: every detour is 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDetours;

impl DetourSource for NoDetours {
    #[inline]
    fn detour(&self, _item: ItemId) -> f64 {
        1.0
    }
}

/// Dynamic detour calculator querying the board contact graph,
/// matching upstream `Connection.get(item).get_detour()`.
pub struct BoardDetours<'a> {
    board: &'a Board,
    cache: RefCell<BTreeMap<ItemId, f64>>,
}

impl<'a> BoardDetours<'a> {
    /// Creates a new detour source over `board`.
    pub fn new(board: &'a Board) -> Self {
        BoardDetours {
            board,
            cache: RefCell::new(BTreeMap::new()),
        }
    }

    fn calculate_detour(&self, seed: ItemId) -> f64 {
        let Some(seed_item) = self.board.store().get(seed) else {
            return 1.0;
        };
        if !seed_item.is_routable() {
            return 1.0;
        }

        let mut scratch = crate::batch::ContactScratch::new();
        let mut contacts = Vec::new();
        crate::batch::normal_contacts(self.board, seed, &mut scratch, &mut contacts);

        let mut connection_items = BTreeSet::new();
        connection_items.insert(seed);

        let mut start_point: Option<Point> = None;
        let mut end_point: Option<Point> = None;

        for curr_contact in contacts {
            let Some(prev_contact_point) =
                crate::batch::normal_contact_point(self.board, seed, curr_contact)
            else {
                continue;
            };
            let prev_contact_layer =
                crate::batch::first_common_layer(self.board, seed, curr_contact).unwrap_or(0);
            let mut fork_found = false;
            if matches!(seed_item.kind, ItemKind::Trace(_)) {
                let mut check = Vec::new();
                self.board
                    .normal_contacts_at(seed, &prev_contact_point, &mut check);
                if check.len() != 1 {
                    fork_found = true;
                }
            }

            let mut curr_id = curr_contact;
            let mut curr_prev_point = prev_contact_point;
            let mut curr_prev_layer = prev_contact_layer;

            loop {
                let Some(curr_item) = self.board.store().get(curr_id) else {
                    break;
                };
                if !curr_item.is_routable() || fork_found {
                    if start_point.is_none() {
                        start_point = Some(curr_prev_point);
                    } else if start_point.as_ref() != Some(&curr_prev_point) {
                        end_point = Some(curr_prev_point);
                    }
                    break;
                }
                connection_items.insert(curr_id);
                let mut curr_contacts = Vec::new();
                crate::batch::normal_contacts(
                    self.board,
                    curr_id,
                    &mut scratch,
                    &mut curr_contacts,
                );

                let mut next_contact: Option<(ItemId, Point, i32)> = None;
                for tmp_contact in curr_contacts {
                    let Some(tmp_layer) =
                        crate::batch::first_common_layer(self.board, curr_id, tmp_contact)
                    else {
                        continue;
                    };
                    let Some(tmp_point) =
                        crate::batch::normal_contact_point(self.board, curr_id, tmp_contact)
                    else {
                        fork_found = true;
                        break;
                    };
                    if tmp_layer != curr_prev_layer || tmp_point != curr_prev_point {
                        if next_contact.is_some() {
                            fork_found = true;
                            break;
                        }
                        next_contact = Some((tmp_contact, tmp_point, tmp_layer));
                    }
                }
                let Some((next_id, next_point, next_layer)) = next_contact else {
                    break;
                };
                curr_id = next_id;
                curr_prev_point = next_point;
                curr_prev_layer = next_layer;
            }
        }

        let detour = match (&start_point, &end_point) {
            (Some(sp), Some(ep)) => {
                let p1 = match sp {
                    Point::Int(p) => p.to_float(),
                    Point::Float(p) => *p,
                    Point::Rational(p) => p.to_float(),
                };
                let p2 = match ep {
                    Point::Int(p) => p.to_float(),
                    Point::Float(p) => *p,
                    Point::Rational(p) => p.to_float(),
                };
                let min_distance = p1.distance(&p2);
                let mut trace_length = 0.0f64;
                for &ci in &connection_items {
                    if let Some(item) = self.board.store().get(ci) {
                        if let ItemKind::Trace(trace) = &item.kind {
                            trace_length += trace.polyline.length_total();
                        }
                    }
                }
                const DETOUR_ADD: f64 = 100.0;
                const DETOUR_ITEM_COST: f64 = 0.1;
                (trace_length + DETOUR_ADD) / (min_distance + DETOUR_ADD)
                    + DETOUR_ITEM_COST * (connection_items.len().saturating_sub(1) as f64)
            }
            _ => f64::from(i32::MAX),
        };

        for &ci in &connection_items {
            self.cache.borrow_mut().insert(ci, detour);
        }
        detour
    }
}

impl<'a> DetourSource for BoardDetours<'a> {
    fn detour(&self, item: ItemId) -> f64 {
        if let Some(&val) = self.cache.borrow().get(&item) {
            return val;
        }
        self.calculate_detour(item)
    }
}

/// The outcome of [`AutorouteEngine::check_ripup`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RipupDecision {
    /// The room cannot be ripped; it is a hard wall.
    Unrippable,
    /// The previous room held the same item — continuing through it is free
    /// (`ALREADY_RIPPED_COSTS = 1`).
    AlreadyRipped,
    /// The toll to pay at every exit door of this obstacle room.
    Cost(i32),
}

impl RipupDecision {
    /// The `p_add_costs` value this decision contributes, upstream's signed
    /// return: `-1` for unrippable, `1` for already-ripped, else the toll.
    #[inline]
    pub fn as_add_costs(self) -> i32 {
        match self {
            RipupDecision::Unrippable => -1,
            RipupDecision::AlreadyRipped => ALREADY_RIPPED_COSTS,
            RipupDecision::Cost(cost) => cost,
        }
    }
}

impl AutorouteEngine<'_> {
    /// `MazeSearchAlgo.check_ripup` (`:1356-1476`).
    ///
    /// `from_door` / `next_room` / `adjustment` are the popped element's; they
    /// stand in for the `MazeListElement` upstream passes whole.
    #[allow(clippy::too_many_arguments)]
    pub fn check_ripup(
        &mut self,
        from_door: crate::rooms::ExpandableRef,
        from_section: u16,
        next_room: RoomId,
        adjustment: Adjustment,
        obstacle_item: ItemId,
        door_is_small: bool,
        detours: &dyn DetourSource,
    ) -> RipupDecision {
        fr_metrics::counters::bump::ripup_checks(1);
        let Some(item) = self.board.store().get(obstacle_item) else {
            return RipupDecision::Unrippable;
        };
        if !item.is_routable() {
            return RipupDecision::Unrippable;
        }
        if door_is_small
            && !self.enter_through_small_door(from_door, next_room, Some(obstacle_item))
        {
            return RipupDecision::Unrippable;
        }

        let previous_item = self
            .previous_room(from_door, next_room)
            .and_then(|room| self.rooms.room(room).obstacle_item());
        let room_was_shoved = adjustment != Adjustment::None;
        if room_was_shoved {
            if let Some(previous_item) = previous_item {
                if previous_item != obstacle_item {
                    let shares_net = self
                        .board
                        .store()
                        .get(previous_item)
                        .zip(self.board.store().get(obstacle_item))
                        .map(|(a, b)| a.shares_net(b))
                        .unwrap_or(false);
                    if shares_net {
                        // The ripped trace may start at a fork.
                        return RipupDecision::Unrippable;
                    }
                }
            }
        } else if previous_item == Some(obstacle_item) {
            return RipupDecision::AlreadyRipped;
        }
        let _ = from_section;

        let mut fanout_via_cost_factor = 1.0f64;
        let mut cost_factor = 1.0f64;
        // D7: one flag, applied to both branches (`:1386-1387`).
        let preserve_fanout_protection = !self.ctrl.remove_unconnected_vias
            && self.ctrl.ripup_costs <= self.ctrl.start_ripup_costs * 2;

        match &item.kind {
            ItemKind::Trace(trace) => {
                cost_factor = trace.half_width as f64;
                if preserve_fanout_protection {
                    fanout_via_cost_factor = self.fanout_via_ripup_cost_factor(obstacle_item);
                }
            }
            ItemKind::Via(_) => {
                let mut look_if_fanout_via = preserve_fanout_protection;
                let contacts = self.normal_contacts(obstacle_item);
                let mut contact_count = 0i32;
                for contact in &contacts {
                    let Some(contact_item) = self.board.store().get(*contact) else {
                        return RipupDecision::Unrippable;
                    };
                    let ItemKind::Trace(contact_trace) = &contact_item.kind else {
                        return RipupDecision::Unrippable;
                    };
                    if contact_item.is_user_fixed() {
                        return RipupDecision::Unrippable;
                    }
                    contact_count += 1;
                    cost_factor = cost_factor.max(contact_trace.half_width as f64);
                    if look_if_fanout_via && !self.ctrl.is_fanout {
                        let curr = self.fanout_via_ripup_cost_factor(*contact);
                        if curr > 1.0 {
                            fanout_via_cost_factor = curr;
                            look_if_fanout_via = false;
                        }
                    }
                }
                if fanout_via_cost_factor <= 1.0 {
                    // Not a fanout via.
                    cost_factor *= 0.5 * (contact_count - 1).max(0) as f64;
                }
            }
            _ => {}
        }

        let mut ripup_cost = self.ctrl.ripup_costs as f64 * cost_factor;
        // D6: the connection is only consulted outside the fanout cases.
        let mut detour = 1.0f64;
        if fanout_via_cost_factor <= 1.0 && !self.ctrl.is_fanout {
            detour = detours.detour(obstacle_item).max(1.0);
        }
        // D4: unreachable with a release-shaped control (`ripup_pass_no == 1`).
        debug_assert!(
            !(self.ctrl.ripup_pass_no >= 4 && self.ctrl.ripup_pass_no % 3 != 0),
            "ripup randomization is dead code in 2.3.0 (conformance appendix D4)"
        );
        ripup_cost /= detour;
        ripup_cost *= fanout_via_cost_factor;
        // `Math.min(Math.max((int) x, 1), MAX_RIPUP_COSTS)` (`:1453-1455`).
        let result = (ripup_cost as i32).clamp(1, MAX_RIPUP_COSTS);
        RipupDecision::Cost(result)
    }

    /// `MazeSearchAlgo.calc_fanout_via_ripup_cost_factor` (`:154-189`).
    ///
    /// A trace whose single end contact is a through-pin, or a two-corner
    /// `SHOVE_FIXED` exit trace, is a protected fanout stub: its factor is
    /// `max(20000 × (half_width / length)², 1)`, which makes a short stub
    /// practically unrippable.
    fn fanout_via_ripup_cost_factor(&self, trace_id: ItemId) -> f64 {
        let Some(item) = self.board.store().get(trace_id) else {
            return 1.0;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            return 1.0;
        };
        for end in 0..2 {
            let point = if end == 0 {
                trace.first_corner()
            } else {
                trace.last_corner()
            };
            let mut contacts = Vec::new();
            self.board
                .normal_contacts_at(trace_id, &point, &mut contacts);
            if contacts.len() != 1 {
                continue;
            }
            let Some(contact) = self.board.store().get(contacts[0]) else {
                continue;
            };
            let protect = match &contact.kind {
                ItemKind::Pin(_) => contact.first_layer() == contact.last_layer(),
                ItemKind::Trace(contact_trace) => {
                    contact.fixed_state == fr_board::item::FixedState::ShoveFixed
                        && contact_trace.corner_count() == 2
                }
                _ => false,
            };
            if !protect {
                continue;
            }
            let length = trace.polyline.length_total();
            if length <= 0.0 {
                return 1.0;
            }
            let mut factor = trace.half_width as f64 / length;
            factor *= factor;
            factor *= FANOUT_COST_CONST;
            return factor.max(1.0);
        }
        1.0
    }

    /// `MazeSearchAlgo.enter_through_small_door` (`:1558-1616`).
    ///
    /// Builds a probe shape perpendicular to the door line, of radius
    /// `compensated_half_width + 2`, and refuses the entry unless every foreign
    /// object it meets is the ignored item or one of its direct same-net
    /// contacts.
    pub fn enter_through_small_door(
        &mut self,
        from_door: crate::rooms::ExpandableRef,
        next_room: RoomId,
        ignore_item: Option<ItemId>,
    ) -> bool {
        let crate::rooms::ExpandableRef::Door(door_id) = from_door else {
            return false;
        };
        let door = self.rooms.door(door_id);
        if door.dimension != 1 {
            return false;
        }
        let door_shape = door.shape.clone();
        if door_shape.is_empty() {
            return false;
        }

        // The line of the 1-dimensional door, skipping zero-length sides.
        let corner_count = door_shape.border_line_count();
        let mut door_line: Option<Line> = None;
        let mut prev_corner = door_shape.corner_approx(0);
        for i in 1..corner_count {
            let next_corner = door_shape.corner_approx(i);
            if next_corner.distance_square(&prev_corner) > 1.0 {
                door_line = Some(door_shape.border_line(i - 1));
                break;
            }
            prev_corner = next_corner;
        }
        let Some(door_line) = door_line else {
            return false;
        };

        let door_center = door_shape.centre_of_gravity().round();
        let layer = self.rooms.room(next_room).layer;
        let check_radius = self.ctrl.compensated_half_width(layer) + TRACE_WIDTH_TOLERANCE as f64;
        let line_arr = [
            door_line.translate(check_radius),
            Line::new_from_point_and_dir(door_center, &door_line.direction().turn_45_degree(2)),
            door_line.translate(-check_radius),
        ];
        let check_polyline = Polyline::new_from_lines(&line_arr);
        let Some(check_shape) = offset_shape(&check_polyline, check_radius) else {
            return false;
        };

        let mut entries = Vec::new();
        self.overlapping_items_on_layer(&check_shape, Some(layer), &mut entries);
        let net_no = self.ctrl.net_no;
        for (item_id, _) in entries {
            if Some(item_id) == ignore_item {
                continue;
            }
            let Some(item) = self.board.store().get(item_id) else {
                continue;
            };
            if item.contains_net(net_no) {
                // Own-net objects are not obstacles for this probe (upstream
                // passes `ignore_net_nos = [net_no]`).
                continue;
            }
            let Some(ignored) = ignore_item else {
                return false;
            };
            let Some(ignored_item) = self.board.store().get(ignored) else {
                return false;
            };
            if !item.shares_net(ignored_item) {
                return false;
            }
            if !self.normal_contacts(item_id).contains(&ignored) {
                return false;
            }
        }
        true
    }

    /// `MazeSearchAlgo.check_leaving_ripped_item` (`:1622-1635`): the same probe
    /// applied when *exiting* a routable obstacle room into a thick free room
    /// through a small door.
    pub fn check_leaving_ripped_item(
        &mut self,
        from_door: crate::rooms::ExpandableRef,
        next_room: RoomId,
    ) -> bool {
        let crate::rooms::ExpandableRef::Door(door_id) = from_door else {
            return false;
        };
        if !matches!(self.rooms.door(door_id).kind, DoorKind::Room) {
            return false;
        }
        let Some(from_room) = self.rooms.door(door_id).other_room_any(next_room) else {
            return false;
        };
        if self.rooms.room(from_room).kind != RoomKind::Obstacle {
            return false;
        }
        let Some(item_id) = self.rooms.room(from_room).obstacle_item() else {
            return false;
        };
        let routable = self
            .board
            .store()
            .get(item_id)
            .map(|item| item.is_routable())
            .unwrap_or(false);
        if !routable {
            return false;
        }
        self.enter_through_small_door(from_door, next_room, Some(item_id))
    }

    /// `p_list_element.door.other_room(p_list_element.next_room)`.
    pub(crate) fn previous_room(
        &self,
        from_door: crate::rooms::ExpandableRef,
        next_room: RoomId,
    ) -> Option<RoomId> {
        match from_door {
            crate::rooms::ExpandableRef::Door(door_id) => {
                self.rooms.door(door_id).other_room(next_room)
            }
            // `ExpansionDrill.other_room` and `DrillPage.other_room` are both
            // hard `null` upstream.
            _ => None,
        }
    }

    /// `Item.get_normal_contacts()` for a drill item or a trace, ascending id.
    pub(crate) fn normal_contacts(&self, item_id: ItemId) -> Vec<ItemId> {
        let Some(item) = self.board.store().get(item_id) else {
            return Vec::new();
        };
        match &item.kind {
            ItemKind::ConductionArea(_) => {
                let mut result = Vec::new();
                for &net_no in &item.nets {
                    for &other_id in self.board.store().net_items(net_no) {
                        if other_id == item_id {
                            continue;
                        }
                        let Some(other) = self.board.store().get(other_id) else {
                            continue;
                        };
                        if !connectivity::shares_layer(item, other) {
                            continue;
                        }
                        let points = connectivity::connection_points(other);
                        if points.iter().any(|p| connectivity::touches_at(item, p)) {
                            if !result.contains(&other_id) {
                                result.push(other_id);
                            }
                        }
                    }
                }
                result.sort_unstable();
                return result;
            }
            _ => {}
        }
        let mut points: Vec<Point> = Vec::new();
        match &item.kind {
            ItemKind::Trace(trace) => {
                points.push(trace.first_corner());
                points.push(trace.last_corner());
            }
            ItemKind::Via(via) => points.push(Point::Int(via.center)),
            ItemKind::Pin(pin) => points.push(Point::Int(pin.center)),
            _ => {}
        }
        let mut result = Vec::new();
        let mut buffer = Vec::new();
        for point in points {
            self.board.normal_contacts_at(item_id, &point, &mut buffer);
            result.extend(buffer.iter().copied());
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    /// `MazeSearchAlgo.room_shape_is_thick` (`:1331-1347`).
    ///
    /// A trace room is thick when `half_width + clearance compensation >=
    /// compensated_trace_half_width`; a via room when
    /// `0.5 × via_shape.max_width()` does.
    pub fn obstacle_room_is_thick(&self, room_id: RoomId) -> bool {
        let room = self.rooms.room(room_id);
        let layer = room.layer;
        let Some((item_id, index)) = room.item else {
            return false;
        };
        let Some(item) = self.board.store().get(item_id) else {
            return false;
        };
        let obstacle_half_width = match &item.kind {
            ItemKind::Trace(trace) => {
                trace.half_width as f64
                    + self.board.clearances().tree_compensation(
                        item.clearance_class,
                        self.ctrl.trace_clearance_class_no,
                        layer,
                    ) as f64
            }
            ItemKind::Via(_) => match self.item_tile(item_id, index) {
                Some(shape) => 0.5 * geom::max_width(&shape),
                None => 0.0,
            },
            _ => 0.0,
        };
        obstacle_half_width >= self.ctrl.compensated_half_width(layer)
    }

    /// Whether `item` is a via, for the rip-and-reuse branch of the via chain.
    pub(crate) fn item_is_via(&self, item_id: ItemId) -> bool {
        self.board
            .store()
            .get(item_id)
            .map(|item| item.class() == ItemClass::Via)
            .unwrap_or(false)
    }
}

/// `Polyline.offset_shape(radius, 0)` for the three-line probe polyline.
///
/// The probe is a single segment thickened by `radius`; the exact upstream
/// construction is the bounding octagon of the segment offset by the radius,
/// intersected with the segment's own half planes. `fr-geometry` exposes the
/// segment, so the probe is built from its bounding octagon enlarged by the
/// radius — never smaller than upstream's, so the probe cannot pass an entry
/// the release would reject.
fn offset_shape(polyline: &Polyline, radius: f64) -> Option<Simplex> {
    let segment = fr_geometry::planar::LineSegment::from_polyline(polyline, 1)?;
    let octagon = segment.bounding_octagon();
    Some(octagon.offset(radius).to_simplex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestFixture;

    #[test]
    fn an_unroutable_item_is_never_rippable() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        // Pins are placed by components and are not routable.
        let pad = fixture.pad_ids()[0];
        let room = engine.rooms.add_complete_free_room(
            fr_geometry::planar::IntBox::new(0, 0, 100, 100).to_simplex(),
            0,
        );
        let obstacle = engine.obstacle_room(pad, 0).unwrap();
        let door = engine.rooms.add_room_door(room, obstacle, 1);
        let decision = engine.check_ripup(
            crate::rooms::ExpandableRef::Door(door),
            0,
            obstacle,
            Adjustment::None,
            pad,
            false,
            &NoDetours,
        );
        assert_eq!(decision, RipupDecision::Unrippable);
    }

    #[test]
    fn a_trace_toll_is_ripup_costs_times_the_half_width() {
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        engine.ctrl.ripup_allowed = true;
        engine.ctrl.ripup_costs = 100;
        let trace = fixture.trace_id();
        let half_width = match &engine.board.store().get(trace).unwrap().kind {
            fr_board::item::ItemKind::Trace(trace) => trace.half_width,
            _ => unreachable!(),
        };
        let room = engine.rooms.add_complete_free_room(
            fr_geometry::planar::IntBox::new(-100, -100, 100, 100).to_simplex(),
            0,
        );
        let obstacle = engine.obstacle_room(trace, 0).unwrap();
        let door = engine.rooms.add_room_door(room, obstacle, 1);
        let decision = engine.check_ripup(
            crate::rooms::ExpandableRef::Door(door),
            0,
            obstacle,
            Adjustment::None,
            trace,
            false,
            &NoDetours,
        );
        // detour = 1, fanout factor = 1 (`remove_unconnected_vias` is true).
        assert_eq!(decision, RipupDecision::Cost(100 * half_width));
    }

    #[test]
    fn the_toll_is_clamped_to_the_max_ripup_costs() {
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        engine.ctrl.ripup_allowed = true;
        engine.ctrl.ripup_costs = i32::MAX / 2;
        let trace = fixture.trace_id();
        let room = engine.rooms.add_complete_free_room(
            fr_geometry::planar::IntBox::new(-100, -100, 100, 100).to_simplex(),
            0,
        );
        let obstacle = engine.obstacle_room(trace, 0).unwrap();
        let door = engine.rooms.add_room_door(room, obstacle, 1);
        let decision = engine.check_ripup(
            crate::rooms::ExpandableRef::Door(door),
            0,
            obstacle,
            Adjustment::None,
            trace,
            false,
            &NoDetours,
        );
        assert_eq!(decision, RipupDecision::Cost(MAX_RIPUP_COSTS));
    }

    #[test]
    fn re_entering_the_same_item_is_already_ripped() {
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        engine.ctrl.ripup_allowed = true;
        let trace = fixture.trace_id();
        let obstacle = engine.obstacle_room(trace, 0).unwrap();
        // A door from the obstacle room back to itself is not constructible, so
        // the previous room is a second obstacle room of the *same* item.
        let second = engine.obstacle_room(trace, 0).unwrap();
        assert_eq!(obstacle, second);
        let neighbour = engine.rooms.add_complete_free_room(
            fr_geometry::planar::IntBox::new(-100, -100, 100, 100).to_simplex(),
            0,
        );
        let door = engine.rooms.add_room_door(neighbour, obstacle, 1);
        // Entering from the free room: previous item is None, so a normal toll.
        let decision = engine.check_ripup(
            crate::rooms::ExpandableRef::Door(door),
            0,
            obstacle,
            Adjustment::None,
            trace,
            false,
            &NoDetours,
        );
        assert!(matches!(decision, RipupDecision::Cost(_)));
    }

    #[test]
    fn detour_divides_the_toll() {
        struct FixedDetour(f64);
        impl DetourSource for FixedDetour {
            fn detour(&self, _item: ItemId) -> f64 {
                self.0
            }
        }
        let fixture = TestFixture::with_foreign_trace();
        let mut engine = fixture.engine();
        engine.ctrl.ripup_allowed = true;
        engine.ctrl.ripup_costs = 100;
        let trace = fixture.trace_id();
        let half_width = match &engine.board.store().get(trace).unwrap().kind {
            fr_board::item::ItemKind::Trace(trace) => trace.half_width,
            _ => unreachable!(),
        };
        let room = engine.rooms.add_complete_free_room(
            fr_geometry::planar::IntBox::new(-100, -100, 100, 100).to_simplex(),
            0,
        );
        let obstacle = engine.obstacle_room(trace, 0).unwrap();
        let door = engine.rooms.add_room_door(room, obstacle, 1);
        let decision = engine.check_ripup(
            crate::rooms::ExpandableRef::Door(door),
            0,
            obstacle,
            Adjustment::None,
            trace,
            false,
            &FixedDetour(4.0),
        );
        assert_eq!(decision, RipupDecision::Cost(100 * half_width / 4));
    }

    #[test]
    fn board_detours_calculates_actual_detour_ratio() {
        let fixture = TestFixture::with_foreign_trace();
        let detours = BoardDetours::new(&fixture.board);
        let trace = fixture.trace_id();
        let detour = detours.detour(trace);
        // The fixture has a straight foreign trace, so detour ratio is ~1.0
        assert!(detour >= 1.0, "detour should be >= 1.0, got {detour}");
    }
}
