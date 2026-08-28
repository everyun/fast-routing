//! The maze loop: `init`, `find_connection`, `occupy_next_element`,
//! `expand_to_room_doors`, the target capture and the three-hop via chain.
//!
//! Transcribed from `autoroute/MazeSearchAlgo.java` (1682 lines — the
//! conformance appendix's §5 port-from table makes this one file the single
//! source for §4, §5, §6, §7.3, §8.1 and §8.3 of `maze-engine.md`).
//!
//! # M2a scope: shove is stubbed
//!
//! Every `MazeShoveTraceAlgo` integration point returns "blocked". Concretely
//! [`MazeSearch::shove_trace_room`] returns `false` unconditionally, which is
//! the same answer the release gives when `check_shove_trace_line` fails on
//! both sides: the element is re-queued at `+ripup_costs` with
//! `already_checked = true` and the section is left unoccupied (the
//! delayed-occupation protocol, `MazeSearchAlgo.java:446-470`), or, when no
//! ripup is possible, the expansion stops there. So M2a explores strictly fewer
//! paths than the release, never more, and every path it does find is one the
//! release could also have found. M2b replaces the one function.
//!
//! `ForcedViaAlgo.check_layer` is likewise M2a-conservative: with no shove
//! engine, a drill layer counts as drillable only when the via pad fits in the
//! free room and meets nothing that is not drillable for the net. See
//! [`MazeSearch::check_layer_with_any_matching_via`].

use fr_board::item::{ItemClass, ItemId, ItemKind};
use fr_geometry::planar::{FloatLine, FloatPoint, IntBox, Point, Simplex};

use crate::ctrl::{ALREADY_RIPPED_COSTS, TRACE_WIDTH_TOLERANCE};
use crate::engine::AutorouteEngine;
use crate::geom;
use crate::maze::heuristic::DestinationDistance;
use crate::maze::nodes::{Adjustment, NodeIndex};
use crate::maze::queue::{Frontier, FrontierElement, FrontierKey, FrontierPayload, FrontierStats};
use crate::maze::ripup::{DetourSource, NoDetours, RipupDecision};
use crate::result::{BacktrackElement, BacktrackKind, FoundConnection, RippedItem};
use crate::rooms::{DoorId, DoorKind, DrillId, ExpandableRef, PageId, RoomId, RoomKind};

/// One settled maze element, in the FRLAB settle-trace format.
///
/// `bench/tools/frlab_parse.py` parses
/// `FR(LAB|METRIC) settle <door_id> <section_no> <g> <f>` from either tool and
/// `diff_settles` compares the two sequences **by index**, reporting a
/// `(door_id, section)` mismatch as a `topology` divergence and a `g`/`f` drift
/// as a `value` divergence. Our ids are our own arena ids, so a cross-tool diff
/// will report `topology` at index 0 on any non-trivial board; what *is*
/// comparable cross-tool is the `g`/`f` sequence and the settle count. Within
/// our own runs the full key is comparable, which is what the determinism test
/// asserts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettleRecord {
    /// The expandable object's arena id.
    pub door_id: u64,
    /// The section index inside it.
    pub section: u32,
    /// `expansion_value`.
    pub g: f64,
    /// `sorting_value`.
    pub f: f64,
}

/// Why [`MazeSearch::find_connection`] stopped without a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFailure {
    /// No destination item had a tile shape to aim at
    /// (`MazeSearchAlgo.java:1240-1244`).
    NoDestination,
    /// No start item produced a non-destination target door
    /// (`MazeSearchAlgo.java:1318-1327`).
    NoStartDoor,
    /// The frontier was exhausted.
    Exhausted,
    /// The caller's stop hook fired.
    Stopped,
}

/// One maze search: one connection of one net.
pub struct MazeSearch<'e, 'b> {
    /// The room database.
    pub engine: &'e mut AutorouteEngine<'b>,
    /// The frontier.
    frontier: Frontier,
    /// The A\* heuristic.
    destination_distance: DestinationDistance,
    /// Start items (`ItemAutorouteInfo.set_start_info(true)`).
    start_items: Vec<ItemId>,
    /// Destination items (`set_start_info(false)`).
    destination_items: Vec<ItemId>,
    /// The destination object, once reached.
    destination_door: Option<(ExpandableRef, u16)>,
    /// Recorded settle sequence, when [`record_settles`](Self::record_settles)
    /// is on.
    settles: Vec<SettleRecord>,
    /// Whether to record the settle sequence in memory.
    record_settles: bool,
    /// Scratch buffer for door section segments.
    section_scratch: Vec<FloatLine>,
    /// Scratch buffer for overlapping drill pages.
    page_scratch: Vec<PageId>,
    /// Cap on settled elements; `None` = unbounded. Stands in for upstream's
    /// `TimeLimit` (`BatchAutorouter.java:1311-1313`), which belongs to the
    /// batch wave.
    expansion_limit: Option<u64>,
    /// Settled elements so far.
    expansions: u64,
}

impl<'e, 'b> MazeSearch<'e, 'b> {
    /// `MazeSearchAlgo.get_instance(start, destination, engine, ctrl)`
    /// (`:118-128`): builds the instance and runs `init`. `Err` mirrors
    /// upstream's `null` return, which the caller reports as `NOT_ROUTED`.
    pub fn get_instance(
        engine: &'e mut AutorouteEngine<'b>,
        start_items: &[ItemId],
        destination_items: &[ItemId],
    ) -> Result<MazeSearch<'e, 'b>, SearchFailure> {
        let destination_distance = DestinationDistance::new(
            &engine.ctrl.trace_costs,
            &engine.ctrl.layer_active,
            engine.ctrl.min_normal_via_cost,
            engine.ctrl.min_cheap_via_cost,
        );
        let mut search = MazeSearch {
            engine,
            frontier: Frontier::new(),
            destination_distance,
            start_items: start_items.to_vec(),
            destination_items: destination_items.to_vec(),
            destination_door: None,
            settles: Vec::new(),
            record_settles: false,
            section_scratch: Vec::new(),
            page_scratch: Vec::new(),
            expansion_limit: None,
            expansions: 0,
        };
        search.init()?;
        Ok(search)
    }

    /// Turns the in-memory settle recording on. The `FRMETRIC settle` trace to
    /// `fr-metrics` is independent of this and is controlled by the
    /// `FR_SETTLE_TRACE` environment variable.
    pub fn record_settles(&mut self, enabled: bool) {
        self.record_settles = enabled;
    }

    /// Caps the number of settled elements, standing in for the per-connection
    /// time limit.
    pub fn set_expansion_limit(&mut self, limit: Option<u64>) {
        self.expansion_limit = limit;
    }

    /// The recorded settle sequence.
    pub fn settles(&self) -> &[SettleRecord] {
        &self.settles
    }

    /// Frontier push/pop counts.
    pub fn frontier_stats(&self) -> FrontierStats {
        self.frontier.stats()
    }

    /// Number of settled elements.
    pub fn expansions(&self) -> u64 {
        self.expansions
    }

    /// `MazeSearchAlgo.find_connection` (`:235-243`), with the backtrack chain
    /// materialized (see [`crate::result`]).
    pub fn find_connection(&mut self) -> Result<FoundConnection, SearchFailure> {
        self.find_connection_with(&NoDetours)
    }

    /// [`find_connection`](Self::find_connection) with an explicit detour
    /// source for the ripup toll.
    pub fn find_connection_with(
        &mut self,
        detours: &dyn DetourSource,
    ) -> Result<FoundConnection, SearchFailure> {
        loop {
            match self.occupy_next_element(detours) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(failure) => return Err(failure),
            }
        }
        match self.destination_door {
            Some((door, section)) => Ok(self.build_result(door, section)),
            None => Err(SearchFailure::Exhausted),
        }
    }

    // ---- initialisation --------------------------------------------------

    /// `MazeSearchAlgo.init` (`:1213-1329`).
    ///
    /// Upstream's step 1, `reduce_trace_shapes_at_tie_pins`, mutates the
    /// compensated tree in place to unblock a multi-net pin's centre. The tree
    /// is shared with the board and this crate does not mutate the board, so
    /// the reduction is deferred to the board-side wave that owns tree
    /// mutation; a tie pin is therefore reachable here only through the
    /// ordinary door machinery.
    fn init(&mut self) -> Result<(), SearchFailure> {
        // Destinations: join their bounding boxes into the heuristic.
        let mut destination_ok = false;
        for item in self.destination_items.clone() {
            let count = self.engine.tree_shape_count(item);
            for index in 0..count as u16 {
                let Some(shape) = self.engine.item_tile(item, index) else {
                    continue;
                };
                let Some(layer) = self.engine.item_tile_layer(item, index) else {
                    continue;
                };
                self.destination_distance.join(&shape.bounding_box(), layer);
            }
            destination_ok = true;
        }
        if !destination_ok && self.engine.ctrl.is_fanout {
            // Fanout needs no destination set: aim at the whole board on both
            // outer layers (`:1232-1238`).
            let bounds = self.engine.bounds();
            let last_layer = self.engine.ctrl.layer_count as i32 - 1;
            self.destination_distance.join(&bounds, 0);
            self.destination_distance.join(&bounds, last_layer);
            destination_ok = true;
        }
        if !destination_ok {
            return Err(SearchFailure::NoDestination);
        }

        // Starts: one incomplete room per connectable tile shape.
        let mut start_rooms = Vec::new();
        for item in self.start_items.clone() {
            let Some(board_item) = self.engine.board.store().get(item) else {
                continue;
            };
            if !board_item.is_connectable() {
                continue;
            }
            let count = self.engine.tree_shape_count(item);
            for index in 0..count as u16 {
                let Some(contained) = self.engine.trace_connection_shape(item, index) else {
                    continue;
                };
                let Some(layer) = self.engine.item_tile_layer(item, index) else {
                    continue;
                };
                start_rooms.push(
                    self.engine
                        .rooms
                        .add_incomplete_room(None, layer, contained),
                );
            }
        }

        let mut completed_start_rooms = Vec::new();
        for room in start_rooms {
            completed_start_rooms.extend(self.engine.complete_expansion_room(room));
        }

        // Seed the frontier from every non-destination target door.
        let mut start_ok = false;
        for room in completed_start_rooms {
            for door_id in self.engine.rooms.room(room).target_doors.clone() {
                let DoorKind::Target {
                    item,
                    tree_entry_no,
                } = self.engine.rooms.door(door_id).kind
                else {
                    continue;
                };
                if self.is_destination_item(item) {
                    continue;
                }
                let Some(connection_shape) =
                    self.engine.trace_connection_shape(item, tree_entry_no)
                else {
                    continue;
                };
                let room_shape = self.engine.rooms.room(room).shape.clone();
                let intersection = connection_shape.intersection(&room_shape);
                if intersection.is_empty() {
                    continue;
                }
                let center = intersection.centre_of_gravity();
                let layer = self.engine.rooms.room(room).layer;
                let sorting_value = self.destination_distance.calculate(&center, layer);
                let node = self
                    .engine
                    .rooms
                    .target_node(door_id, &mut self.engine.nodes);
                let _ = node;
                self.frontier.push(
                    FrontierKey {
                        f: sorting_value,
                        g: 0.0,
                        object_id: self.engine.rooms.door(door_id).oid,
                        section: 0,
                    },
                    FrontierPayload {
                        door: ExpandableRef::Door(door_id),
                        section_no_of_door: 0,
                        backtrack_door: None,
                        section_no_of_backtrack_door: 0,
                        next_room: Some(room),
                        shape_entry: FloatLine::new(center, center),
                        room_ripped: false,
                        adjustment: Adjustment::None,
                        already_checked: false,
                        ripup_cost: 0,
                    },
                );
                start_ok = true;
            }
        }
        if start_ok {
            Ok(())
        } else {
            Err(SearchFailure::NoStartDoor)
        }
    }

    #[inline]
    fn is_destination_item(&self, item: ItemId) -> bool {
        self.destination_items.contains(&item)
    }

    // ---- the main loop ---------------------------------------------------

    /// `MazeSearchAlgo.occupy_next_element` (`:249-317`).
    ///
    /// `Ok(true)` = keep going, `Ok(false)` = the destination was reached or the
    /// frontier is exhausted, `Err` = the stop rule fired.
    pub fn occupy_next_element(
        &mut self,
        detours: &dyn DetourSource,
    ) -> Result<bool, SearchFailure> {
        if self.destination_door.is_some() {
            return Ok(false);
        }
        let element = loop {
            if let Some(limit) = self.expansion_limit {
                if self.expansions >= limit {
                    return Err(SearchFailure::Stopped);
                }
            }
            let Some(element) = self.frontier.pop() else {
                return Ok(false);
            };
            let Some(node) =
                self.section_node(element.payload.door, element.payload.section_no_of_door)
            else {
                self.frontier.note_stale_pop();
                continue;
            };
            if !self.engine.nodes.is_occupied(node) {
                break (element, node);
            }
            self.frontier.note_stale_pop();
        };
        let (element, node) = element;

        // Settle: the backtrack payload lands now, the occupied flag maybe.
        self.engine.nodes.settle(
            node,
            element.payload.backtrack_door,
            element.payload.section_no_of_backtrack_door,
            element.payload.room_ripped,
            element.payload.ripup_cost,
            element.payload.adjustment,
        );
        self.expansions += 1;
        fr_metrics::counters::bump::expansions(1);
        self.emit_settle(&element);

        // `:283-286`: a drill page expands and returns *without* occupying.
        if let ExpandableRef::Page(_) = element.payload.door {
            self.expand_to_drills_of_page(&element);
            return Ok(true);
        }

        // `:288-295`: a destination target door ends the search.
        if let ExpandableRef::Door(door_id) = element.payload.door {
            if let DoorKind::Target { item, .. } = self.engine.rooms.door(door_id).kind {
                if self.is_destination_item(item) {
                    self.destination_door =
                        Some((element.payload.door, element.payload.section_no_of_door));
                    return Ok(false);
                }
            }
        }

        // `:296-302`: in fanout mode a drill reached from a drill ends it.
        let door_is_drill = matches!(element.payload.door, ExpandableRef::Drill(_));
        let backtrack_is_drill = matches!(
            element.payload.backtrack_door,
            Some(ExpandableRef::Drill(_))
        );
        if self.engine.ctrl.is_fanout && door_is_drill && backtrack_is_drill {
            self.destination_door =
                Some((element.payload.door, element.payload.section_no_of_door));
            return Ok(false);
        }

        // `:303-306`.
        if self.engine.ctrl.vias_allowed && door_is_drill && !backtrack_is_drill {
            self.expand_to_other_layers(&element);
        }

        // `:308-314`: the delayed-occupation return path.
        if element.payload.next_room.is_some() && !self.expand_to_room_doors(&element, detours) {
            return Ok(true);
        }
        self.engine.nodes.occupy(node);
        Ok(true)
    }

    fn emit_settle(&mut self, element: &FrontierElement) {
        let record = SettleRecord {
            door_id: element.key.object_id as u64,
            section: element.key.section as u32,
            g: element.key.g,
            f: element.key.f,
        };
        fr_metrics::settle_trace::settle(record.door_id, record.section, record.g, record.f);
        if self.record_settles {
            self.settles.push(record);
        }
    }

    // ---- per-room expansion ---------------------------------------------

    /// `MazeSearchAlgo.expand_to_room_doors` (`:323-528`).
    ///
    /// Returns `true` when the from-section must be occupied, `false` when the
    /// occupation is delayed (or nothing was expanded and the next room is thin,
    /// so another section may still claim it).
    fn expand_to_room_doors(
        &mut self,
        element: &FrontierElement,
        detours: &dyn DetourSource,
    ) -> bool {
        let Some(next_room) = element.payload.next_room else {
            return true;
        };
        if !self.engine.rooms.room(next_room).alive {
            return true;
        }
        let layer_no = self.engine.rooms.room(next_room).layer;
        let layer_active = self.engine.ctrl.layer_is_active(layer_no);
        if !layer_active && self.engine.board.layers().is_signal(layer_no) {
            // `:329-334`: a dead *signal* layer is a dead end.
            return true;
        }

        let mut half_width = self.engine.ctrl.compensated_half_width(layer_no);
        let mut curr_door_is_small = false;
        if let ExpandableRef::Door(door_id) = element.payload.door {
            if matches!(self.engine.rooms.door(door_id).kind, DoorKind::Room) {
                let mut half_width_add = half_width + TRACE_WIDTH_TOLERANCE as f64;
                if self.engine.ctrl.with_neckdown {
                    // `:340-347`. `check_neck_down_at_dest_pin` needs the pin's
                    // neckdown half width, which lives with the pin-exit
                    // geometry of the locate/insert wave; M2a reports 0 = "no
                    // neckdown", which is upstream's answer for every pin
                    // without a neckdown rule.
                    let neck_down = self.check_neck_down_at_dest_pin(next_room);
                    if neck_down > 0.0 {
                        half_width_add = half_width_add.min(neck_down);
                        half_width = half_width_add;
                    }
                }
                curr_door_is_small = self.door_is_small(door_id, 2.0 * half_width_add);
            }
        }

        // Freeze this room's door list (`:352`).
        self.engine.complete_neighbour_rooms(next_room);
        if !self.engine.rooms.room(next_room).alive {
            return true;
        }

        let shape_entry_middle = geom::midpoint(&element.payload.shape_entry);

        // `:377-398`: thin/thick.
        let next_room_is_thick = if self.engine.rooms.room(next_room).kind == RoomKind::Obstacle {
            self.engine.obstacle_room_is_thick(next_room)
        } else {
            let room_shape = self.engine.rooms.room(next_room).shape.clone();
            if geom::min_width(&room_shape) < 2.0 * half_width {
                false
            } else if !element.payload.already_checked
                && self.object_dimension(element.payload.door) == 1
                && !curr_door_is_small
            {
                let nearest = room_shape.nearest_border_points_approx(&shape_entry_middle, 2);
                if nearest.len() < 2 {
                    false
                } else {
                    nearest[1].distance(&shape_entry_middle) > half_width + 1.0
                }
            } else {
                true
            }
        };

        // `:399-409`: the split-plane rule.
        if !layer_active {
            if let ExpandableRef::Drill(drill_id) = element.payload.door {
                let location = self.engine.drills.get(drill_id).location;
                if self.foreign_conduction_at(location, layer_no) {
                    return true;
                }
            }
        }

        let mut something_expanded = self.expand_to_target_doors(
            element,
            next_room,
            next_room_is_thick,
            curr_door_is_small,
            &shape_entry_middle,
        );

        // `:413-415`: a plane layer carries targets only.
        if !layer_active {
            return true;
        }

        let mut ripup_costs = 0i32;
        match self.engine.rooms.room(next_room).kind {
            RoomKind::CompleteFree | RoomKind::IncompleteFree => {
                if !element.payload.already_checked && curr_door_is_small {
                    let mut enter_through_small_door = false;
                    if next_room_is_thick {
                        enter_through_small_door = self
                            .engine
                            .check_leaving_ripped_item(element.payload.door, next_room);
                    }
                    if !enter_through_small_door {
                        return something_expanded;
                    }
                }
            }
            RoomKind::Obstacle => {
                if !element.payload.already_checked {
                    let Some(obstacle_item) = self.engine.rooms.room(next_room).obstacle_item()
                    else {
                        return true;
                    };
                    let mut room_rippable = false;
                    if self.engine.ctrl.ripup_allowed {
                        let decision = self.engine.check_ripup(
                            element.payload.door,
                            element.payload.section_no_of_door,
                            next_room,
                            element.payload.adjustment,
                            obstacle_item,
                            curr_door_is_small,
                            detours,
                        );
                        ripup_costs = decision.as_add_costs().max(0);
                        room_rippable = decision != RipupDecision::Unrippable;
                        if decision == RipupDecision::Unrippable {
                            ripup_costs = 0;
                        }
                    }

                    let is_trace = self
                        .engine
                        .board
                        .store()
                        .get(obstacle_item)
                        .map(|item| item.class() == ItemClass::Trace)
                        .unwrap_or(false);
                    if ripup_costs != ALREADY_RIPPED_COSTS
                        && next_room_is_thick
                        && !curr_door_is_small
                        && self.engine.ctrl.max_shove_trace_recursion_depth > 0
                        && is_trace
                        && !self.shove_trace_room(element, next_room)
                    {
                        if ripup_costs > 0 {
                            // Delay the occupation by ripup so another door
                            // section may still shove the room (`:448-465`).
                            self.requeue_delayed(element, ripup_costs);
                        }
                        return something_expanded;
                    }
                    if !room_rippable {
                        return true;
                    }
                }
            }
        }

        // `:476-500`: a snapshot, because completion may mutate the list.
        let doors_snapshot = self.engine.rooms.room(next_room).doors.clone();
        for to_door in doors_snapshot {
            if ExpandableRef::Door(to_door) == element.payload.door {
                continue;
            }
            if self.expand_to_door(to_door, element, next_room, ripup_costs, next_room_is_thick) {
                something_expanded = true;
            }
        }

        // `:502-525`: the via expansion.
        if self.engine.ctrl.vias_allowed && !matches!(element.payload.door, ExpandableRef::Drill(_))
        {
            let room_kind = self.engine.rooms.room(next_room).kind;
            if (something_expanded || next_room_is_thick) && room_kind == RoomKind::CompleteFree {
                let room_shape = self.engine.rooms.room(next_room).shape.clone();
                self.engine
                    .pages
                    .overlapping_pages(&room_shape, &mut self.page_scratch);
                let pages = std::mem::take(&mut self.page_scratch);
                for page in &pages {
                    self.expand_to_drill_page(*page, element, next_room);
                    something_expanded = true;
                }
                self.page_scratch = pages;
            } else if room_kind == RoomKind::Obstacle {
                if let Some(item) = self.engine.rooms.room(next_room).obstacle_item() {
                    if self.engine.item_is_via(item) {
                        // Rip-and-reuse of an existing via site. Building the
                        // via's own `ExpansionDrill` needs
                        // `Via.get_autoroute_drill_info`, which is the drill
                        // cache keyed by the via rather than by a page; M2a
                        // reaches an existing via through the page grid
                        // instead, so this branch is a no-op and the only cost
                        // is that a via can be reused one hop later.
                    }
                }
            }
        }

        something_expanded
    }

    /// `MazeSearchAlgo.expand_to_target_doors` (`:534-597`).
    fn expand_to_target_doors(
        &mut self,
        element: &FrontierElement,
        next_room: RoomId,
        next_room_is_thick: bool,
        curr_door_is_small: bool,
        shape_entry_middle: &FloatPoint,
    ) -> bool {
        if curr_door_is_small {
            // Entering through a small door is only allowed when coming out of
            // an obstacle room (`:536-549`).
            let mut ok = false;
            if let ExpandableRef::Door(door_id) = element.payload.door {
                if matches!(self.engine.rooms.door(door_id).kind, DoorKind::Room) {
                    if let Some(from_room) = self.engine.rooms.door(door_id).other_room(next_room) {
                        ok = self.engine.rooms.room(from_room).kind == RoomKind::Obstacle;
                    }
                }
            }
            if !ok {
                return false;
            }
        }
        let mut result = false;
        for to_door in self.engine.rooms.room(next_room).target_doors.clone() {
            if ExpandableRef::Door(to_door) == element.payload.door {
                continue;
            }
            let DoorKind::Target {
                item,
                tree_entry_no,
            } = self.engine.rooms.door(to_door).kind
            else {
                continue;
            };
            // Conformance appendix D12: the stale-target guard is in the
            // release (`:556-567`).
            if tree_entry_no as usize >= self.engine.tree_shape_count(item) {
                continue;
            }
            let Some(target_shape) = self.engine.trace_connection_shape(item, tree_entry_no) else {
                continue;
            };
            let connection_point = target_shape.nearest_point_approx(shape_entry_middle);
            if !next_room_is_thick {
                // `:570-588`: verify a real trace can be forced from the entry
                // to the connection point. That is `check_forced_trace_polyline`,
                // i.e. the shove engine; with shove stubbed, M2a admits the hop
                // only when the straight segment stays inside the room, which
                // is the no-shove case of the same predicate.
                if !self.straight_segment_fits(next_room, shape_entry_middle, &connection_point) {
                    continue;
                }
            }
            let new_shape_entry = FloatLine::new(connection_point, connection_point);
            if self.expand_to_door_section(
                ExpandableRef::Door(to_door),
                0,
                new_shape_entry,
                element,
                next_room,
                0,
                Adjustment::None,
            ) {
                result = true;
            }
        }
        result
    }

    /// `MazeSearchAlgo.expand_to_door` (`:602-647`).
    fn expand_to_door(
        &mut self,
        to_door: DoorId,
        element: &FrontierElement,
        next_room: RoomId,
        add_costs: i32,
        next_room_is_thick: bool,
    ) -> bool {
        let layer = self.engine.rooms.room(next_room).layer;
        let half_width = self.engine.ctrl.compensated_half_width(layer);
        let mut sections = std::mem::take(&mut self.section_scratch);
        let count = self.engine.rooms.section_segments(
            to_door,
            half_width,
            &mut self.engine.nodes,
            &mut sections,
        );
        let mut something_expanded = false;
        let mut rejected = false;
        for i in 0..count as usize {
            let Some(section) = sections.get(i).copied() else {
                break;
            };
            let Some(node) = self.engine.rooms.section_node(to_door, i as u16) else {
                continue;
            };
            if self.engine.nodes.is_occupied(node) {
                continue;
            }
            let new_shape_entry = if next_room_is_thick {
                let door = self.engine.rooms.door(to_door);
                if door.dimension == 1
                    && count == 1
                    && self.engine.rooms.room(door.first_room).kind == RoomKind::CompleteFree
                    && self.engine.rooms.room(door.second_room).kind == RoomKind::CompleteFree
                {
                    // The acute-corner squeeze check (`:615-629`) rejects the
                    // *whole* door, not just this section.
                    let entry_middle = geom::midpoint(&section);
                    let room_shape = self.engine.rooms.room(next_room).shape.clone();
                    if geom::min_width(&room_shape) < 2.0 * half_width {
                        rejected = true;
                        break;
                    }
                    let nearest = room_shape.nearest_border_points_approx(&entry_middle, 2);
                    if nearest.len() < 2 || nearest[1].distance(&entry_middle) <= half_width + 1.0 {
                        rejected = true;
                        break;
                    }
                }
                section
            } else {
                // `:630-639`: a thin room only lets a trace pass straight
                // through, and a point-like first section is skipped.
                if self.engine.rooms.door(to_door).dimension == 1
                    && i == 0
                    && section.b.distance_square(&section.a) < 1.0
                {
                    continue;
                }
                match geom::segment_projection(&element.payload.shape_entry, &section) {
                    Some(projection) => projection,
                    None => continue,
                }
            };
            if self.expand_to_door_section(
                ExpandableRef::Door(to_door),
                i as u16,
                new_shape_entry,
                element,
                next_room,
                add_costs,
                Adjustment::None,
            ) {
                something_expanded = true;
            }
        }
        self.section_scratch = sections;
        if rejected {
            return false;
        }
        something_expanded
    }

    /// `MazeSearchAlgo.door_is_small` (`:653-677`).
    ///
    /// The board's angle restriction picks the length measure: the bounding
    /// box's `max_width` at 90°, the bounding octagon's at 45°, the diagonal
    /// corner segment's length otherwise.
    fn door_is_small(&self, door_id: DoorId, trace_width: f64) -> bool {
        let door = self.engine.rooms.door(door_id);
        let both_free = self.engine.rooms.room(door.first_room).kind == RoomKind::CompleteFree
            && self.engine.rooms.room(door.second_room).kind == RoomKind::CompleteFree;
        if door.dimension != 1 && !both_free {
            return false;
        }
        if door.shape.is_empty() {
            return true;
        }
        let door_length = match self.engine.board.angle_restriction() {
            fr_board::shape::AngleRestriction::Ninety => {
                fr_geometry::planar::ConvexShape::max_width(&door.shape.bounding_box())
            }
            fr_board::shape::AngleRestriction::FortyFive => match door.shape.bounding_octagon() {
                Some(octagon) => fr_geometry::planar::ConvexShape::max_width(&octagon),
                None => return true,
            },
            fr_board::shape::AngleRestriction::None => match &door.door_line {
                Some(line) => line.b.distance(&line.a),
                None => match geom::diagonal_corner_segment(&door.shape) {
                    Some(line) => line.b.distance(&line.a),
                    None => return true,
                },
            },
        };
        door_length < trace_width
    }

    /// `MazeSearchAlgo.expand_to_door_section` (`:682-790`), including the bend
    /// penalty of conformance appendix §4(a).
    #[allow(clippy::too_many_arguments)]
    fn expand_to_door_section(
        &mut self,
        door: ExpandableRef,
        section_no: u16,
        shape_entry: FloatLine,
        from_element: &FrontierElement,
        from_room: RoomId,
        add_costs: i32,
        adjustment: Adjustment,
    ) -> bool {
        let Some(node) = self.section_node(door, section_no) else {
            return false;
        };
        if self.engine.nodes.is_occupied(node) {
            return false;
        }
        let next_room = match door {
            ExpandableRef::Door(door_id) => self.engine.rooms.door(door_id).other_room(from_room),
            _ => None,
        };
        let layer = self.engine.rooms.room(from_room).layer;
        let shape_entry_middle = geom::midpoint(&shape_entry);
        let from_mid = geom::midpoint(&from_element.payload.shape_entry);

        let mut bend_cost_penalty = 0.0;
        let bend_cost = self
            .engine
            .ctrl
            .bend_costs
            .get(layer.max(0) as usize)
            .copied()
            .unwrap_or(0.0);
        if bend_cost > 0.0 {
            if let Some(backtrack) = from_element.payload.backtrack_door {
                let backtrack_cog = self.object_shape_cog(backtrack);
                let prev_dx = from_mid.x - backtrack_cog.x;
                let prev_dy = from_mid.y - backtrack_cog.y;
                let next_dx = shape_entry_middle.x - from_mid.x;
                let next_dy = shape_entry_middle.y - from_mid.y;
                let cross_product = prev_dx * next_dy - prev_dy * next_dx;
                let sq_len_prev = prev_dx * prev_dx + prev_dy * prev_dy;
                let sq_len_next = next_dx * next_dx + next_dy * next_dy;
                if sq_len_prev > 0.0
                    && sq_len_next > 0.0
                    && (cross_product * cross_product) > 0.01 * sq_len_prev * sq_len_next
                {
                    bend_cost_penalty = bend_cost;
                }
            }
        }

        let cost = self.engine.ctrl.trace_cost(layer);
        let expansion_value = from_element.key.g
            + add_costs as f64
            + bend_cost_penalty
            + shape_entry_middle.weighted_distance(&from_mid, cost.horizontal, cost.vertical);
        let sorting_value = expansion_value
            + self
                .destination_distance
                .calculate(&shape_entry_middle, layer);
        let room_ripped = (add_costs > 0 && adjustment == Adjustment::None)
            || (from_element.payload.already_checked && from_element.payload.room_ripped);
        let ripup_cost = if add_costs > 0 && adjustment == Adjustment::None {
            add_costs
        } else {
            0
        };

        if !self.fanout_gate_allows(door, next_room, &shape_entry) {
            return false;
        }

        self.frontier.push(
            FrontierKey {
                f: sorting_value,
                g: expansion_value,
                object_id: self.object_oid(door),
                section: section_no,
            },
            FrontierPayload {
                door,
                section_no_of_door: section_no,
                backtrack_door: Some(from_element.payload.door),
                section_no_of_backtrack_door: from_element.payload.section_no_of_door,
                next_room,
                shape_entry,
                room_ripped,
                adjustment,
                already_checked: false,
                ripup_cost,
            },
        );
        true
    }

    /// The delayed-occupation requeue (`:448-465`).
    fn requeue_delayed(&mut self, element: &FrontierElement, ripup_costs: i32) {
        let mut payload = element.payload;
        payload.already_checked = true;
        payload.room_ripped = true;
        payload.ripup_cost = ripup_costs;
        self.frontier.push(
            FrontierKey {
                f: element.key.f + ripup_costs as f64,
                g: element.key.g + ripup_costs as f64,
                object_id: element.key.object_id,
                section: element.key.section,
            },
            payload,
        );
    }

    /// `MazeSearchAlgo.shove_trace_room` (`:1484-1535`) — **M2a stub**.
    ///
    /// Returns `false` unconditionally: "the obstacle is hard". The caller
    /// therefore takes the delayed-ripup branch, which is exactly what the
    /// release does when `MazeShoveTraceAlgo.check_shove_trace_line` fails on
    /// both sides. M2b replaces this body with the real shove and must also
    /// start producing [`Adjustment::Left`] / [`Adjustment::Right`] elements
    /// (everything downstream already carries them).
    fn shove_trace_room(&mut self, _element: &FrontierElement, _obstacle_room: RoomId) -> bool {
        fr_metrics::counters::bump::shove_checks(1);
        false
    }

    /// `MazeSearchAlgo.check_neck_down_at_dest_pin` (`:1542-1550`) — M2a
    /// returns 0, i.e. "no destination pin needs neckdown".
    fn check_neck_down_at_dest_pin(&self, _room: RoomId) -> f64 {
        0.0
    }

    /// The no-shove case of `board.check_forced_trace_polyline`: does the
    /// straight segment from the entry to the connection point stay inside the
    /// room?
    fn straight_segment_fits(&self, room: RoomId, from: &FloatPoint, to: &FloatPoint) -> bool {
        let from_point = from.round();
        let to_point = to.round();
        if from_point == to_point {
            return true;
        }
        let shape = &self.engine.rooms.room(room).shape;
        shape.contains(&Point::Int(from_point)) && shape.contains(&Point::Int(to_point))
    }

    /// The split-plane check of `:399-409`: is there a foreign-net conduction
    /// area at the drill location on this layer?
    fn foreign_conduction_at(&self, location: fr_geometry::planar::IntPoint, layer: i32) -> bool {
        let mut picked = Vec::new();
        self.engine
            .board
            .pick_items(&Point::Int(location), Some(layer), &mut picked);
        picked.into_iter().any(|id| {
            self.engine
                .board
                .store()
                .get(id)
                .map(|item| {
                    matches!(item.kind, ItemKind::ConductionArea(_))
                        && !item.contains_net(self.engine.ctrl.net_no)
                })
                .unwrap_or(false)
        })
    }

    /// The fanout escape-length gate that upstream hides inside its `TreeSet`
    /// subclass (`MazeSearchAlgo.java:80-108`; conformance appendix §3 item 2).
    /// `maze-engine.md` §12.2 asks for it at the push sites, which is here.
    fn fanout_gate_allows(
        &self,
        door: ExpandableRef,
        next_room: Option<RoomId>,
        shape_entry: &FloatLine,
    ) -> bool {
        if !self.engine.ctrl.is_fanout {
            return true;
        }
        let Some(pin_center) = self.engine.ctrl.fanout_start_pin_center else {
            return true;
        };
        let pin_center = pin_center.to_float();
        if let (Some(max_len), Some(room)) = (self.engine.ctrl.fanout_max_escape_length, next_room)
        {
            if self.engine.rooms.room(room).layer == self.engine.ctrl.fanout_start_pin_layer {
                let entry = geom::midpoint(shape_entry);
                if entry.distance(&pin_center) > max_len {
                    return false;
                }
            }
        }
        if let (Some(min_len), ExpandableRef::Drill(drill_id)) =
            (self.engine.ctrl.fanout_min_escape_length, door)
        {
            let location = self.engine.drills.get(drill_id).location.to_float();
            if location.distance(&pin_center) < min_len {
                return false;
            }
        }
        true
    }

    // ---- the via chain ---------------------------------------------------

    /// Hop 1: room → drill page (`MazeSearchAlgo.expand_to_drill_page`,
    /// `:955-980`).
    ///
    /// The via cost is paid here; the travel to the page counts only for
    /// ordering, so it lands in `f` and not in `g`.
    fn expand_to_drill_page(
        &mut self,
        page_id: PageId,
        element: &FrontierElement,
        next_room: RoomId,
    ) {
        let layer = self.engine.rooms.room(next_room).layer;
        let Some(node) = self
            .engine
            .pages
            .page_node(page_id, layer, &mut self.engine.nodes)
        else {
            return;
        };
        if self.engine.nodes.is_occupied(node) {
            return;
        }
        let from_middle = geom::midpoint(&element.payload.shape_entry);
        let page_shape = self.engine.pages.page(page_id).shape;
        let nearest_point = nearest_box_point(&page_shape, &from_middle);
        let expansion_value = element.key.g + self.engine.ctrl.min_normal_via_cost;
        let cost = self.engine.ctrl.trace_cost(layer);
        let sorting_value = expansion_value
            + nearest_point.weighted_distance(&from_middle, cost.horizontal, cost.vertical)
            + self.destination_distance.calculate(&nearest_point, layer);
        let oid = self.engine.pages.page(page_id).oid;
        self.frontier.push(
            FrontierKey {
                f: sorting_value,
                g: expansion_value,
                object_id: oid,
                section: layer.max(0) as u16,
            },
            FrontierPayload {
                door: ExpandableRef::Page(page_id),
                section_no_of_door: layer.max(0) as u16,
                backtrack_door: Some(element.payload.door),
                section_no_of_backtrack_door: element.payload.section_no_of_door,
                next_room: Some(next_room),
                shape_entry: element.payload.shape_entry,
                room_ripped: element.payload.room_ripped,
                adjustment: Adjustment::None,
                already_checked: false,
                ripup_cost: 0,
            },
        );
    }

    /// Hop 2: the popped page enumerates its drills
    /// (`MazeSearchAlgo.expand_to_drills_of_page`, `:982-1039`).
    ///
    /// A drill is accepted only when its room on the page's layer **is** the
    /// room the page was entered from and its layer node is free.
    fn expand_to_drills_of_page(&mut self, element: &FrontierElement) {
        let ExpandableRef::Page(page_id) = element.payload.door else {
            return;
        };
        let from_room_layer = element.payload.section_no_of_door as i32;
        let attach_smd = self.engine.ctrl.attach_smd_allowed;
        let drills = self.engine.page_drills(page_id, attach_smd);
        for drill_id in drills {
            let section_no = from_room_layer - self.engine.drills.get(drill_id).first_layer;
            if section_no < 0
                || section_no >= self.engine.drills.get(drill_id).room_arr.len() as i32
            {
                continue;
            }
            if Some(self.engine.drills.get(drill_id).room_arr[section_no as usize])
                != element.payload.next_room
            {
                continue;
            }
            let Some(node) = self
                .engine
                .drills
                .get(drill_id)
                .section_node(section_no as u16)
            else {
                continue;
            };
            if self.engine.nodes.is_occupied(node) {
                continue;
            }
            self.expand_to_drill(drill_id, element, 0);
        }
    }

    /// Hop 3: page → drill (`MazeSearchAlgo.expand_to_drill`, `:872-948`).
    fn expand_to_drill(&mut self, drill_id: DrillId, element: &FrontierElement, add_costs: i32) {
        let Some(from_room) = element.payload.next_room else {
            return;
        };
        let layer = self.engine.rooms.room(from_room).layer;
        let trace_half_width = self.engine.ctrl.compensated_half_width(layer);
        let room_shape = self.engine.rooms.room(from_room).shape.clone();
        let room_shape_is_thin = geom::min_width(&room_shape) < 2.0 * trace_half_width;
        let drill_shape = self.engine.drills.get(drill_id).shape.clone();

        if room_shape_is_thin {
            // Straight-through constraint: only drills meeting the backtrack
            // door are allowed (`:879-891`).
            let Some(backtrack) = element.payload.backtrack_door else {
                return;
            };
            let backtrack_shape = self.object_shape(backtrack);
            if drill_shape.intersection(&backtrack_shape).dimension() < 0 {
                return;
            }
            if !drill_shape.intersects(&backtrack_shape) {
                return;
            }
        }

        let via_radius = self
            .engine
            .ctrl
            .via_radius_arr
            .get(layer.max(0) as usize)
            .copied()
            .unwrap_or(0.0);
        let shrinked = drill_shape.shrink(via_radius);
        // `:897-911`: coming from a pin's target door via a page, the compare
        // corner is the pin's nearest trace exit corner. That geometry lives
        // with the locate/insert wave; M2a uses the entry midpoint, which is
        // upstream's answer for every non-pin source.
        let compare_corner = geom::midpoint(&element.payload.shape_entry);
        let nearest_point = if shrinked.is_empty() {
            drill_shape.nearest_point_approx(&compare_corner)
        } else {
            shrinked.nearest_point_approx(&compare_corner)
        };
        let shape_entry = FloatLine::new(nearest_point, nearest_point);
        let section_no = layer - self.engine.drills.get(drill_id).first_layer;
        if section_no < 0 {
            return;
        }
        let cost = self.engine.ctrl.trace_cost(layer);
        let mut expansion_value = element.key.g
            + add_costs as f64
            + nearest_point.weighted_distance(&compare_corner, cost.horizontal, cost.vertical);

        let (backtrack_door, section_no_of_backtrack_door) =
            if matches!(element.payload.door, ExpandableRef::Page(_)) {
                // The backtrack skips the page; the via cost was paid at hop 1.
                (
                    element.payload.backtrack_door,
                    element.payload.section_no_of_backtrack_door,
                )
            } else {
                // Expanded directly through an existing via: hop 1 was skipped,
                // so the via cost is paid here (`:922-928`).
                expansion_value += self.engine.ctrl.min_normal_via_cost;
                (
                    Some(element.payload.door),
                    element.payload.section_no_of_door,
                )
            };
        let sorting_value =
            expansion_value + self.destination_distance.calculate(&nearest_point, layer);
        let oid = self.engine.drills.get(drill_id).oid;
        let door = ExpandableRef::Drill(drill_id);
        if !self.fanout_gate_allows(door, None, &shape_entry) {
            return;
        }
        self.frontier.push(
            FrontierKey {
                f: sorting_value,
                g: expansion_value,
                object_id: oid,
                section: section_no as u16,
            },
            FrontierPayload {
                door,
                section_no_of_door: section_no as u16,
                backtrack_door,
                section_no_of_backtrack_door,
                // A drill expands specially, so it carries no next room.
                next_room: None,
                shape_entry,
                room_ripped: element.payload.room_ripped,
                adjustment: Adjustment::None,
                already_checked: false,
                ripup_cost: 0,
            },
        );
    }

    /// Hop 4: the popped drill fans out to the other layers
    /// (`MazeSearchAlgo.expand_to_other_layers`, `:1044-1180`).
    fn expand_to_other_layers(&mut self, element: &FrontierElement) {
        let ExpandableRef::Drill(drill_id) = element.payload.door else {
            return;
        };
        let (first_layer, last_layer, room_arr, location) = {
            let drill = self.engine.drills.get(drill_id);
            (
                drill.first_layer,
                drill.last_layer,
                drill.room_arr.clone(),
                drill.location,
            )
        };
        let from_section = element.payload.section_no_of_door as i32;
        let from_layer = first_layer + from_section;
        let via_lower_bound;
        let via_upper_bound;
        let mut smd_attached_on_component_side = false;
        let mut smd_attached_on_solder_side = false;
        let room_ripped;

        let from_room = room_arr.get(from_section.max(0) as usize).copied();
        let from_room_is_obstacle = from_room
            .map(|room| self.engine.rooms.room(room).kind == RoomKind::Obstacle)
            .unwrap_or(false);

        if from_room_is_obstacle {
            // Via ripup (`:1052-1068`).
            if !self.engine.ctrl.ripup_allowed {
                return;
            }
            let Some(item_id) =
                from_room.and_then(|room| self.engine.rooms.room(room).obstacle_item())
            else {
                return;
            };
            let Some(item) = self.engine.board.store().get(item_id) else {
                return;
            };
            let ItemKind::Via(via) = &item.kind else {
                return;
            };
            // `ctrl.via_rule.contains_padstack(padstack)` compares padstack
            // identity; `fr-board`'s padstacks carry no name, so the rule is
            // matched on the layer span the padstack occupies plus the
            // clearance class, which is what the identity stands for here.
            let via_first = via.layers.first;
            let via_last = via.layers.last;
            let padstack_in_rule = self
                .engine
                .ctrl
                .via_info_arr
                .iter()
                .any(|mask| mask.from_layer == via_first && mask.to_layer == via_last);
            if !padstack_in_rule || item.clearance_class != self.engine.ctrl.via_clearance_class {
                return;
            }
            via_lower_bound = via.layers.first;
            via_upper_bound = via.layers.last;
            room_ripped = true;
        } else {
            room_ripped = false;
            let via_lower_limit = first_layer.max(self.engine.ctrl.via_lower_bound);
            let via_upper_limit = last_layer.min(self.engine.ctrl.via_upper_bound);
            // Scan down.
            let mut curr_layer = from_layer;
            loop {
                let Some(room) = room_arr.get((curr_layer - first_layer).max(0) as usize) else {
                    via_lower_bound = curr_layer + 1;
                    break;
                };
                let room_shape = self.engine.rooms.room(*room).shape.clone();
                match self.check_layer_with_any_matching_via(&location, curr_layer, &room_shape) {
                    CheckDrillResult::NotDrillable => {
                        via_lower_bound = curr_layer + 1;
                        break;
                    }
                    CheckDrillResult::DrillableWithAttachSmd => {
                        if curr_layer == 0 {
                            smd_attached_on_component_side = true;
                        } else if curr_layer == self.engine.ctrl.layer_count as i32 - 1 {
                            smd_attached_on_solder_side = true;
                        }
                    }
                    CheckDrillResult::Drillable => {}
                }
                if curr_layer <= via_lower_limit {
                    via_lower_bound = via_lower_limit;
                    break;
                }
                curr_layer -= 1;
            }
            // The drill only fires where the whole free-space column is
            // drillable (`:1098-1100`).
            if via_lower_bound > first_layer {
                return;
            }

            // Scan up.
            let mut curr_layer = from_layer + 1;
            loop {
                if curr_layer > via_upper_limit {
                    via_upper_bound = via_upper_limit;
                    break;
                }
                let Some(room) = room_arr.get((curr_layer - first_layer).max(0) as usize) else {
                    via_upper_bound = curr_layer - 1;
                    break;
                };
                let room_shape = self.engine.rooms.room(*room).shape.clone();
                match self.check_layer_with_any_matching_via(&location, curr_layer, &room_shape) {
                    CheckDrillResult::NotDrillable => {
                        via_upper_bound = curr_layer - 1;
                        break;
                    }
                    CheckDrillResult::DrillableWithAttachSmd => {
                        if curr_layer == self.engine.ctrl.layer_count as i32 - 1 {
                            smd_attached_on_solder_side = true;
                        }
                    }
                    CheckDrillResult::Drillable => {}
                }
                curr_layer += 1;
            }
            if via_upper_bound < last_layer {
                return;
            }
        }

        for to_layer in via_lower_bound..=via_upper_bound {
            if to_layer == from_layer {
                continue;
            }
            let (curr_first_layer, curr_last_layer) = if to_layer < from_layer {
                (to_layer, from_layer)
            } else {
                (from_layer, to_layer)
            };
            let mask_found = self.engine.ctrl.via_info_arr.iter().any(|mask| {
                if curr_first_layer < mask.from_layer
                    || curr_last_layer > mask.to_layer
                    || mask.from_layer < via_lower_bound
                    || mask.to_layer > via_upper_bound
                {
                    return false;
                }
                let needs_attach = (mask.from_layer == 0 && smd_attached_on_component_side)
                    || (mask.to_layer == self.engine.ctrl.layer_count as i32 - 1
                        && smd_attached_on_solder_side);
                !needs_attach || mask.attach_smd_allowed
            });
            if !mask_found {
                continue;
            }
            let curr_room_index = to_layer - first_layer;
            let Some(node) = self
                .engine
                .drills
                .get(drill_id)
                .section_node(curr_room_index.max(0) as u16)
            else {
                continue;
            };
            if self.engine.nodes.is_occupied(node) {
                continue;
            }
            let add_via_cost = self
                .engine
                .ctrl
                .add_via_costs
                .get(from_layer.max(0) as usize)
                .and_then(|row| row.get(to_layer.max(0) as usize))
                .copied()
                .unwrap_or(0);
            let expansion_value = element.key.g + add_via_cost as f64;
            let shape_entry_middle = geom::midpoint(&element.payload.shape_entry);
            let sorting_value = expansion_value
                + self
                    .destination_distance
                    .calculate(&shape_entry_middle, to_layer);
            let Some(next_room) = room_arr.get(curr_room_index.max(0) as usize).copied() else {
                continue;
            };
            let oid = self.engine.drills.get(drill_id).oid;
            self.frontier.push(
                FrontierKey {
                    f: sorting_value,
                    g: expansion_value,
                    object_id: oid,
                    section: curr_room_index as u16,
                },
                FrontierPayload {
                    door: ExpandableRef::Drill(drill_id),
                    section_no_of_door: curr_room_index as u16,
                    backtrack_door: Some(ExpandableRef::Drill(drill_id)),
                    section_no_of_backtrack_door: element.payload.section_no_of_door,
                    next_room: Some(next_room),
                    shape_entry: element.payload.shape_entry,
                    room_ripped,
                    adjustment: Adjustment::None,
                    already_checked: false,
                    ripup_cost: 0,
                },
            );
        }
    }

    /// `MazeSearchAlgo.check_layer_with_any_matching_via` (`:1182-1207`;
    /// conformance appendix §3 item 5): try **every** via of the rule with its
    /// own radius, clearance class and attach flag; `DRILLABLE` on the first
    /// success, else `DRILLABLE_WITH_ATTACH_SMD` if any padstack got that.
    ///
    /// `ForcedViaAlgo.check_layer` itself is the shove engine's entry point.
    /// M2a's stand-in is the no-shove case: the via pad must fit inside the
    /// free room and must meet nothing that is not drillable for the routed
    /// net, with a drillable pin counting as attach-SMD.
    fn check_layer_with_any_matching_via(
        &mut self,
        location: &fr_geometry::planar::IntPoint,
        layer: i32,
        room_shape: &Simplex,
    ) -> CheckDrillResult {
        let mut drillable_with_attach_smd = false;
        // The candidate list is snapshotted first: `check_layer` needs `&mut
        // self` for the tree query, and the via rule lives inside `self.ctrl`.
        let trace_half_width = self
            .engine
            .ctrl
            .trace_half_width
            .get(layer.max(0) as usize)
            .copied()
            .unwrap_or(0) as f64;
        let candidates: Vec<(f64, bool)> = (0..self.engine.ctrl.via_rule.via_count())
            .filter_map(|index| {
                let via = self.engine.ctrl.via_rule.get_via(index)?;
                let padstack = via.padstack();
                if (layer as usize) < padstack.from_layer()
                    || (layer as usize) > padstack.to_layer()
                {
                    return None;
                }
                let via_radius = padstack
                    .get_shape(layer as usize)
                    .map(|shape| 0.5 * shape.max_width())
                    .unwrap_or(0.0);
                Some((via_radius.max(trace_half_width), via.attach_smd_allowed()))
            })
            .collect();
        for (required_radius, attach_smd_allowed) in candidates {
            match self.check_layer(location, layer, room_shape, required_radius) {
                CheckDrillResult::Drillable => return CheckDrillResult::Drillable,
                CheckDrillResult::DrillableWithAttachSmd => {
                    if attach_smd_allowed {
                        drillable_with_attach_smd = true;
                    }
                }
                CheckDrillResult::NotDrillable => {}
            }
        }
        if drillable_with_attach_smd {
            CheckDrillResult::DrillableWithAttachSmd
        } else {
            CheckDrillResult::NotDrillable
        }
    }

    fn check_layer(
        &mut self,
        location: &fr_geometry::planar::IntPoint,
        layer: i32,
        room_shape: &Simplex,
        required_radius: f64,
    ) -> CheckDrillResult {
        let pad = Point::Int(*location)
            .surrounding_octagon()
            .offset(required_radius)
            .to_simplex();
        if !geom::contains_shape(room_shape, &pad) {
            return CheckDrillResult::NotDrillable;
        }
        let mut entries = Vec::new();
        self.engine
            .overlapping_items_on_layer(&pad, Some(layer), &mut entries);
        let net_no = self.engine.ctrl.net_no;
        let mut attach_smd = false;
        for (item_id, _) in entries {
            let Some(item) = self.engine.board.store().get(item_id) else {
                continue;
            };
            if item.is_drillable(net_no) {
                continue;
            }
            if item.class() == ItemClass::Pin && item.drill_allowed() {
                attach_smd = true;
                continue;
            }
            return CheckDrillResult::NotDrillable;
        }
        if attach_smd {
            CheckDrillResult::DrillableWithAttachSmd
        } else {
            CheckDrillResult::Drillable
        }
    }

    // ---- expandable-object accessors -------------------------------------

    fn object_oid(&self, object: ExpandableRef) -> u32 {
        match object {
            ExpandableRef::Door(door) => self.engine.rooms.door(door).oid,
            ExpandableRef::Drill(drill) => self.engine.drills.get(drill).oid,
            ExpandableRef::Page(page) => self.engine.pages.page(page).oid,
        }
    }

    fn object_dimension(&self, object: ExpandableRef) -> i32 {
        match object {
            ExpandableRef::Door(door) => self.engine.rooms.door(door).dimension,
            // `ExpansionDrill.get_dimension` and `DrillPage.get_dimension` are
            // both a constant 2.
            _ => 2,
        }
    }

    fn object_shape(&self, object: ExpandableRef) -> Simplex {
        match object {
            ExpandableRef::Door(door) => self.engine.rooms.door(door).shape.clone(),
            ExpandableRef::Drill(drill) => self.engine.drills.get(drill).shape.clone(),
            ExpandableRef::Page(page) => self.engine.pages.page(page).shape.to_simplex(),
        }
    }

    fn object_shape_cog(&self, object: ExpandableRef) -> FloatPoint {
        self.object_shape(object).centre_of_gravity()
    }

    fn section_node(&mut self, object: ExpandableRef, section: u16) -> Option<NodeIndex> {
        match object {
            ExpandableRef::Door(door) => {
                if matches!(self.engine.rooms.door(door).kind, DoorKind::Target { .. }) {
                    return Some(self.engine.rooms.target_node(door, &mut self.engine.nodes));
                }
                self.engine.rooms.section_node(door, section)
            }
            ExpandableRef::Drill(drill) => self.engine.drills.get(drill).section_node(section),
            ExpandableRef::Page(page) => {
                self.engine
                    .pages
                    .page_node(page, section as i32, &mut self.engine.nodes)
            }
        }
    }

    // ---- the backtrack ---------------------------------------------------

    /// `LocateFoundConnectionAlgo.backtrack` (`pull-tight-insert.md` §2.2),
    /// materialized into [`FoundConnection`].
    fn build_result(&mut self, door: ExpandableRef, section: u16) -> FoundConnection {
        let mut backtrack = Vec::new();
        let mut ripped_items: Vec<RippedItem> = Vec::new();

        let mut cur_door = door;
        let mut cur_section = section;
        let mut next_room = match cur_door {
            ExpandableRef::Door(door_id) => Some(self.engine.rooms.door(door_id).first_room),
            ExpandableRef::Drill(drill_id) => {
                let drill = self.engine.drills.get(drill_id);
                drill.room_arr.get(cur_section as usize).copied()
            }
            ExpandableRef::Page(_) => None,
        };
        // A ripped drill contributes every obstacle room of its column.
        if let ExpandableRef::Drill(drill_id) = cur_door {
            if let Some(node) = self.section_node(cur_door, cur_section) {
                let state = self.engine.nodes.get(node);
                if state.room_ripped {
                    let rooms = self.engine.drills.get(drill_id).room_arr.clone();
                    for room in rooms {
                        if let Some(item) = self.engine.rooms.room(room).obstacle_item() {
                            push_ripped(&mut ripped_items, item, state.ripup_cost);
                        }
                    }
                }
            }
        }

        loop {
            let Some(node) = self.section_node(cur_door, cur_section) else {
                break;
            };
            let state = self.engine.nodes.get(node);
            backtrack.push(self.backtrack_element(cur_door, cur_section, next_room, &state));

            let Some(previous) = state.backtrack else {
                break;
            };
            let previous_section = state.section_no_of_backtrack;
            next_room = match previous {
                ExpandableRef::Drill(drill_id) => self
                    .engine
                    .drills
                    .get(drill_id)
                    .room_arr
                    .get(previous_section as usize)
                    .copied(),
                ExpandableRef::Door(door_id) => match next_room {
                    Some(room) => self
                        .engine
                        .rooms
                        .door(door_id)
                        .other_room(room)
                        .or(Some(self.engine.rooms.door(door_id).first_room)),
                    None => Some(self.engine.rooms.door(door_id).first_room),
                },
                ExpandableRef::Page(_) => next_room,
            };
            cur_door = previous;
            cur_section = previous_section;

            if let Some(room) = next_room {
                let Some(next_node) = self.section_node(cur_door, cur_section) else {
                    continue;
                };
                let next_state = self.engine.nodes.get(next_node);
                if next_state.room_ripped {
                    if let Some(item) = self.engine.rooms.room(room).obstacle_item() {
                        push_ripped(&mut ripped_items, item, next_state.ripup_cost);
                    }
                }
            }
            if backtrack.len() > self.engine.nodes.len() + 2 {
                // Defensive: a cycle in the backtrack chain would hang the
                // locator. Upstream has no guard; ours truncates instead.
                break;
            }
        }

        FoundConnection {
            destination_door: door,
            section_no_of_destination_door: section,
            backtrack,
            ripped_items,
        }
    }

    fn backtrack_element(
        &self,
        door: ExpandableRef,
        section: u16,
        next_room: Option<RoomId>,
        state: &crate::maze::nodes::MazeNodeState,
    ) -> BacktrackElement {
        let kind = match door {
            ExpandableRef::Door(door_id) => match self.engine.rooms.door(door_id).kind {
                DoorKind::Room => BacktrackKind::RoomDoor,
                DoorKind::Target {
                    item,
                    tree_entry_no,
                } => BacktrackKind::TargetDoor {
                    item,
                    tree_entry_no,
                },
            },
            ExpandableRef::Drill(drill_id) => {
                let drill = self.engine.drills.get(drill_id);
                BacktrackKind::Drill {
                    location: drill.location,
                    first_layer: drill.first_layer,
                    last_layer: drill.last_layer,
                }
            }
            // A page never appears in the chain: hop 3 skips it (`:919-921`).
            ExpandableRef::Page(_) => BacktrackKind::RoomDoor,
        };
        let next_room = next_room.filter(|room| self.engine.rooms.room(*room).alive);
        BacktrackElement {
            door,
            section_no_of_door: section,
            kind,
            door_shape: self.object_shape(door),
            next_room,
            next_room_shape: next_room.map(|room| self.engine.rooms.room(room).shape.clone()),
            next_room_layer: next_room.map(|room| self.engine.rooms.room(room).layer),
            next_room_is_obstacle: next_room
                .map(|room| self.engine.rooms.room(room).kind == RoomKind::Obstacle)
                .unwrap_or(false),
            room_ripped: state.room_ripped,
            ripup_cost: state.ripup_cost,
            adjustment: state.adjustment,
        }
    }
}

fn push_ripped(list: &mut Vec<RippedItem>, item: ItemId, ripup_cost: i32) {
    if list.iter().any(|entry| entry.item == item) {
        return;
    }
    list.push(RippedItem { item, ripup_cost });
}

/// `IntBox.nearest_point(FloatPoint)` — the clamp of a point into a box.
fn nearest_box_point(boundary: &IntBox, point: &FloatPoint) -> FloatPoint {
    FloatPoint::new(
        point
            .x
            .clamp(boundary.left_x() as f64, boundary.right_x() as f64),
        point
            .y
            .clamp(boundary.bottom_y() as f64, boundary.top_y() as f64),
    )
}

/// `ForcedPadAlgo.CheckDrillResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckDrillResult {
    NotDrillable,
    Drillable,
    DrillableWithAttachSmd,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestFixture;

    #[test]
    fn init_fails_without_a_destination() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let pads = fixture.pad_ids();
        let failure = MazeSearch::get_instance(&mut engine, &pads[..1], &[]).err();
        assert_eq!(failure, Some(SearchFailure::NoDestination));
    }

    #[test]
    fn init_seeds_one_element_per_start_target_door() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let pads = fixture.pad_ids();
        let search = MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2])
            .expect("start and destination exist");
        assert!(search.frontier_stats().pushes >= 1);
    }

    #[test]
    fn the_settle_trace_is_recorded_when_asked() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut engine = fixture.engine();
        let pads = fixture.pad_ids();
        let mut search =
            MazeSearch::get_instance(&mut engine, &pads[..1], &pads[1..2]).expect("instance");
        search.record_settles(true);
        let _ = search.find_connection();
        assert!(!search.settles().is_empty());
        assert_eq!(search.settles().len() as u64, search.expansions());
    }
}
