//! `board/ShapeTraceEntries.java` (725 L) — the entry-point / stack-level
//! calculus that decides whether the items overlapping one shove shape can be
//! pushed aside, and what the substitute traces around the shape look like.
//!
//! This is the part of the shove stack that `shove.md` §15 calls
//! "bit-for-bit behaviour worth preserving": CCW sorted insertion with the
//! scalar-product tie-break, the resort around the from-side with the
//! `edge_no += edge_count` wrap, the run-collapse rule, the bracket-nesting
//! stack levels behind the `stack_depth <= 1` gate, and outermost-first
//! `pop_piece`.
//!
//! # Deviations from the Java shape, and why they are not deviations
//!
//! * the singly linked `EntryPoint` list is a `Vec<EntryPoint>` with `u32 next`
//!   indices. Splicing (which is all the list is for) is the same pointer
//!   rewrite; the arena means one allocation per cascade instead of one per
//!   intersection (§15, "Arena the shove transaction").
//! * an entry stores the obstacle trace's *line* rather than a reference to the
//!   trace plus a line index. `next_substitute_trace_piece` is the only reader
//!   and it only ever wants `trace.polyline().arr[trace_line_no]`, so the
//!   polyline never has to be cloned.
//! * a substitute piece comes back as a [`SubstitutePiece`] plan — net,
//!   half width, clearance class, polyline — not as a board item. Only the
//!   insert path materializes one, on commit.

use fr_geometry::planar::{FloatPoint, Line, Point, Polyline, Simplex};

use super::from_side::FromSide;
use super::geom;
use crate::board::Board;
use crate::clearance::ClearanceSource;
use crate::item::{FixedState, Item, ItemId, ItemKind, ObstacleKind};
use crate::net_nos::NetNos;
use crate::shape::TileShape;

/// `ShapeTraceEntries.c_offset_add` (`:18`) — one board unit of safety on every
/// offset shape the shove builds.
pub const C_OFFSET_ADD: f64 = 1.0;

const NONE: u32 = u32::MAX;

/// One intersection of an obstacle trace's polyline with the offset border of
/// the shove shape (`ShapeTraceEntries.EntryPoint`, `:709-725`).
#[derive(Debug, Clone, Copy)]
struct EntryPoint {
    /// Index into [`ShapeTraceEntries::traces`].
    trace_ref: u32,
    /// `trace.polyline().arr[trace_line_no]`, the only thing the substitute
    /// piece builder wants from the trace.
    line: Line,
    entry_approx: FloatPoint,
    /// May exceed the border line count after `resort` — that is the wrap
    /// marker, and `next_substitute_trace_piece` relies on it.
    edge_no: i32,
    stack_level: i32,
    next: u32,
}

/// What an entry needs to know about the trace it came from.
#[derive(Debug, Clone)]
struct TraceInfo {
    id: ItemId,
    nets: NetNos,
    half_width: i32,
    cl_class: i32,
}

/// A substitute trace piece, as a plan rather than a board item.
#[derive(Debug, Clone)]
pub struct SubstitutePiece {
    /// The re-routed centre line, hugging the offset border of the shove shape.
    pub polyline: Polyline,
    /// The layer the obstacle ran on.
    pub layer: i32,
    /// The obstacle's half width — a substitute keeps the trace it replaces.
    pub half_width: i32,
    /// The obstacle's nets.
    pub nets: NetNos,
    /// The obstacle's clearance class.
    pub cl_class: i32,
}

impl SubstitutePiece {
    /// `PolylineTrace.tile_shape_count()`: one tile shape per interior line.
    pub fn tile_shape_count(&self) -> usize {
        self.polyline.arr.len().saturating_sub(2)
    }

    /// `Item.get_compensated_half_width(search_tree)`.
    pub fn compensated_half_width(&self, board: &Board) -> i32 {
        compensated_half_width(board, self.half_width, self.cl_class, self.layer)
    }
}

/// `Item.get_compensated_half_width(search_tree)` (`Item.java:954`).
pub fn compensated_half_width(board: &Board, half_width: i32, cl_class: i32, layer: i32) -> i32 {
    if board.trees().clearance_compensation_used() {
        half_width + board.clearances().compensation_value(cl_class, layer)
    } else {
        half_width
    }
}

/// The shove shape grown by an obstacle trace's reach, `c_offset_add` included.
///
/// `ShapeTraceEntries.java:265-282`, verbatim: one compensated step when the
/// tree carries the clearance, otherwise **two** steps —
/// `offset(half_width)` then `offset(clearance + 1)` — "for symmetry reasons".
/// The comment two lines down ("using enlarge here instead offset causes
/// problems because of a comparison in the constructor of class EntryPoint")
/// is why this is `offset`, which translates every border line, and not
/// `enlarge`, which additionally clips to the bounding octagon and would move
/// the edge indices the entry list is sorted on.
pub fn offset_shape_for_trace(
    board: &Board,
    shape: &Simplex,
    trace_half_width: i32,
    trace_cl_class: i32,
    trace_layer: i32,
    own_cl_class: i32,
) -> Simplex {
    if board.trees().clearance_compensation_used() {
        let curr_offset =
            compensated_half_width(board, trace_half_width, trace_cl_class, trace_layer) as f64
                + C_OFFSET_ADD;
        shape.offset(curr_offset)
    } else {
        let cl_offset = board
            .clearances()
            .value(trace_cl_class, own_cl_class, trace_layer) as f64
            + C_OFFSET_ADD;
        shape.offset(trace_half_width as f64).offset(cl_offset)
    }
}

/// The tile shape of `id` on `layer` in the default tree, upstream's
/// `Item.get_tree_shape_on_layer`.
pub fn tree_shape_on_layer(board: &Board, id: ItemId, layer: i32) -> Option<TileShape> {
    let slot = board.store().slot_of(id)?;
    board
        .default_tree()
        .item_shapes(slot, id)
        .iter()
        .find(|tile| tile.layer == layer)
        .and_then(|tile| tile.shape.clone())
}

/// `ShapeTraceEntries` — one instance per shove shape.
#[derive(Debug)]
pub struct ShapeTraceEntries {
    /// Foreign vias overlapping the shape, in descending id order.
    pub shove_via_list: Vec<ItemId>,
    shape: Simplex,
    layer: i32,
    own_net_nos: NetNos,
    cl_class: i32,
    from_side: Option<FromSide>,
    list_anchor: u32,
    entries: Vec<EntryPoint>,
    traces: Vec<TraceInfo>,
    trace_piece_count: i32,
    max_stack_level: i32,
    shape_contains_trace_tails: bool,
    found_obstacle: Option<ItemId>,
}

impl ShapeTraceEntries {
    /// `ShapeTraceEntries(shape, layer, own_net_nos, cl_type, from_side, board)`
    /// (`:34-46`). `from_side` of `None` is upstream's `null`, and a `no < 0`
    /// is `NOT_CALCULATED`; both are resolved lazily by `search_from_side`.
    pub fn new(
        shape: Simplex,
        layer: i32,
        own_net_nos: NetNos,
        cl_class: i32,
        from_side: Option<FromSide>,
    ) -> ShapeTraceEntries {
        ShapeTraceEntries {
            shove_via_list: Vec::new(),
            shape,
            layer,
            own_net_nos,
            cl_class,
            from_side,
            list_anchor: NONE,
            entries: Vec::new(),
            traces: Vec::new(),
            trace_piece_count: 0,
            max_stack_level: 0,
            shape_contains_trace_tails: false,
            found_obstacle: None,
        }
    }

    /// `stack_depth()` — the nesting depth of foreign traces in the shape.
    /// Every check path treats `> 1` as failure.
    pub fn stack_depth(&self) -> i32 {
        self.max_stack_level
    }

    /// `substitute_trace_count()`.
    pub fn substitute_trace_count(&self) -> i32 {
        self.trace_piece_count
    }

    /// `trace_tails_in_shape()` — only the *static* maze-time check reads this.
    pub fn trace_tails_in_shape(&self) -> bool {
        self.shape_contains_trace_tails
    }

    /// `get_found_obstacle()` — the blame item for the UI.
    pub fn found_obstacle(&self) -> Option<ItemId> {
        self.found_obstacle
    }

    /// `store_items(item_list, is_pad_check, copper_sharing_allowed)`
    /// (`:130-171`). Returns false when the list holds an obstacle that cannot
    /// be shoved aside.
    pub fn store_items(
        &mut self,
        board: &Board,
        item_list: &[ItemId],
        is_pad_check: bool,
        copper_sharing_allowed: bool,
    ) -> bool {
        for &id in item_list {
            let Some(item) = board.store().get(id) else {
                continue;
            };
            // Java precedence: `(!is_pad_check && ViaObstacleArea) ||
            // ComponentObstacleArea` — a component keepout is always skipped.
            if let ItemKind::ObstacleArea(area) = &item.kind {
                if (!is_pad_check && area.kind == ObstacleKind::ViaKeepout)
                    || area.kind == ObstacleKind::ComponentKeepout
                {
                    continue;
                }
            }
            let contains_own_net = item.nets.shares_any(&self.own_net_nos);
            if let ItemKind::ConductionArea(area) = &item.kind {
                if contains_own_net || !area.is_obstacle {
                    continue;
                }
            }
            if item.is_shove_fixed() && !contains_own_net {
                self.found_obstacle = Some(id);
                return false;
            }
            match &item.kind {
                ItemKind::Via(_) => {
                    if is_pad_check || !contains_own_net {
                        self.shove_via_list.push(id);
                    }
                }
                ItemKind::Trace(_) => {
                    if !self.store_trace(board, id, item) {
                        return false;
                    }
                }
                _ => {
                    if contains_own_net {
                        if !copper_sharing_allowed {
                            self.found_obstacle = Some(id);
                            return false;
                        }
                        if is_pad_check
                            && !(matches!(item.kind, ItemKind::Pin(_)) && item.drill_allowed())
                        {
                            self.found_obstacle = Some(id);
                            return false;
                        }
                    } else {
                        self.found_obstacle = Some(id);
                        return false;
                    }
                }
            }
        }
        self.search_from_side();
        self.resort();
        self.calculate_stack_levels()
    }

    /// `store_trace(trace)` (`:265-370`) — the entry-point computation.
    fn store_trace(&mut self, board: &Board, id: ItemId, item: &Item) -> bool {
        let ItemKind::Trace(trace) = &item.kind else {
            return true;
        };
        let offset_shape = offset_shape_for_trace(
            board,
            &self.shape,
            trace.half_width,
            item.clearance_class,
            trace.layer,
            self.cl_class,
        );
        if offset_shape.is_empty() {
            return true;
        }

        let trace_ref = self.trace_ref(id, item, trace.half_width);

        for (trace_line_no, border_line_no) in offset_shape.entrance_points(&trace.polyline) {
            let Some(&trace_line) = trace.polyline.arr.get(trace_line_no) else {
                continue;
            };
            let entry_approx =
                trace_line.intersection_approx(&offset_shape.border_line(border_line_no));
            self.insert_entry_point(trace_ref, trace_line, border_line_no as i32, entry_approx);
        }

        // A trace end inside the shape has to be spliced, not cut — only for
        // foreign nets (`:290-368`).
        if item.nets.shares_any(&self.own_net_nos) {
            self.found_obstacle = Some(id);
            return true;
        }
        if !item.nets_normal() {
            return false;
        }

        let mut contacts = Vec::new();
        let mut end_corner = trace.first_corner();
        for i in 0..2 {
            if offset_shape.contains(&end_corner) {
                board.normal_contacts_at(id, &end_corner, &mut contacts);
                let mut contact_count = 0usize;
                let mut store_end_corner = true;
                for &contact_id in contacts.iter() {
                    let Some(contact) = board.store().get(contact_id) else {
                        continue;
                    };
                    if !contact.is_routable() {
                        self.found_obstacle = Some(contact_id);
                        return false;
                    }
                    match &contact.kind {
                        ItemKind::Trace(contact_trace) => {
                            if (contact.is_shove_fixed()
                                || contact_trace.half_width != trace.half_width
                                || contact.clearance_class != item.clearance_class)
                                && offset_shape.contains_inside(&end_corner)
                            {
                                self.found_obstacle = Some(contact_id);
                                return false;
                            }
                        }
                        ItemKind::Via(_) => {
                            let Some(via_shape) =
                                tree_shape_on_layer(board, contact_id, self.layer)
                            else {
                                contact_count += 1;
                                continue;
                            };
                            let via_simplex = geom::tile_to_simplex(&via_shape);
                            let mut via_trace_diff = via_simplex.smallest_radius()
                                - compensated_half_width(
                                    board,
                                    trace.half_width,
                                    item.clearance_class,
                                    trace.layer,
                                ) as f64;
                            if !board.trees().clearance_compensation_used() {
                                let via_clearance = board.clearances().value(
                                    contact.clearance_class,
                                    self.cl_class,
                                    self.layer,
                                );
                                let trace_clearance = board.clearances().value(
                                    item.clearance_class,
                                    self.cl_class,
                                    self.layer,
                                );
                                if trace_clearance > via_clearance {
                                    via_trace_diff += (via_clearance - trace_clearance) as f64;
                                }
                            }
                            if via_trace_diff < 0.0 {
                                // The via is thinner than the trace: it cannot
                                // stand in for the trace end.
                                self.found_obstacle = Some(contact_id);
                                return false;
                            }
                            if via_trace_diff == 0.0 && !offset_shape.contains_inside(&end_corner) {
                                store_end_corner = false;
                            }
                        }
                        _ => {}
                    }
                    contact_count += 1;
                }
                if contact_count == 1 && store_end_corner {
                    if let Some(projection) = geom::nearest_border_point(&offset_shape, &end_corner)
                    {
                        if let Some(projection_side) =
                            geom::contains_on_border_line_no(&offset_shape, &projection)
                        {
                            // "may not be correct because the trace may not
                            // contain a suitable line for the construction of
                            // the end line of the substitute trace" (`:349`).
                            let trace_line_segment_no =
                                if i == 0 { 0 } else { trace.polyline.arr.len() - 1 };
                            if let Some(&trace_line) = trace.polyline.arr.get(trace_line_segment_no)
                            {
                                self.insert_entry_point(
                                    trace_ref,
                                    trace_line,
                                    projection_side as i32,
                                    geom::point_to_float(&projection),
                                );
                            }
                        }
                    }
                } else if contact_count == 0 && offset_shape.contains_inside(&end_corner) {
                    self.shape_contains_trace_tails = true;
                }
            }
            end_corner = trace.last_corner();
        }
        self.found_obstacle = Some(id);
        true
    }

    fn trace_ref(&mut self, id: ItemId, item: &Item, half_width: i32) -> u32 {
        if let Some(index) = self.traces.iter().position(|info| info.id == id) {
            return index as u32;
        }
        self.traces.push(TraceInfo {
            id,
            nets: item.nets.clone(),
            half_width,
            cl_class: item.clearance_class,
        });
        (self.traces.len() - 1) as u32
    }

    /// `insert_entry_point` (`:669-704`) — sorted insertion by `edge_no`, ties
    /// broken by the scalar-product projection along the edge, i.e. CCW
    /// arc-length order around the border.
    fn insert_entry_point(
        &mut self,
        trace_ref: u32,
        line: Line,
        edge_no: i32,
        entry_approx: FloatPoint,
    ) {
        let new_index = self.entries.len() as u32;
        let edge_count = self.shape.border_line_count();
        let mut curr_prev = NONE;
        let mut curr_next = self.list_anchor;
        while curr_next != NONE {
            let next = self.entries[curr_next as usize];
            if next.edge_no > edge_no {
                break;
            }
            if next.edge_no == edge_no && edge_count > 0 {
                let side = (edge_no as usize).min(edge_count - 1);
                let prev_corner = self.shape.corner_approx(side);
                let next_corner = if side == edge_count - 1 {
                    self.shape.corner_approx(0)
                } else {
                    self.shape.corner_approx(side + 1)
                };
                if prev_corner.scalar_product(&entry_approx, &next_corner)
                    <= prev_corner.scalar_product(&next.entry_approx, &next_corner)
                {
                    break;
                }
            }
            curr_prev = curr_next;
            curr_next = next.next;
        }
        self.entries.push(EntryPoint {
            trace_ref,
            line,
            entry_approx,
            edge_no,
            stack_level: -1,
            next: curr_next,
        });
        if curr_prev != NONE {
            self.entries[curr_prev as usize].next = new_index;
        } else {
            self.list_anchor = new_index;
        }
    }

    /// `search_from_side()` (`:372-388`): with no legal from-side, take the edge
    /// of the first own-net entry.
    fn search_from_side(&mut self) {
        if let Some(from_side) = self.from_side {
            if from_side.no >= 0 {
                return;
            }
        }
        let mut curr = self.list_anchor;
        let mut curr_fromside_no = 0;
        let mut curr_entry_approx = None;
        while curr != NONE {
            let entry = self.entries[curr as usize];
            if self.traces[entry.trace_ref as usize]
                .nets
                .shares_any(&self.own_net_nos)
            {
                curr_fromside_no = entry.edge_no;
                curr_entry_approx = Some(entry.entry_approx);
                break;
            }
            curr = entry.next;
        }
        self.from_side = Some(FromSide::raw(curr_fromside_no, curr_entry_approx));
    }

    fn nets_of(&self, entry_index: u32) -> &NetNos {
        &self.traces[self.entries[entry_index as usize].trace_ref as usize].nets
    }

    /// `resort()` (`:393-513`): rotate the list so it starts in the middle of
    /// the from-side, collapse runs of the same net set, and strip own-net
    /// entries from both ends.
    fn resort(&mut self) {
        let edge_count = self.shape.border_line_count() as i32;
        let Some(mut from_side) = self.from_side else {
            return;
        };
        if from_side.no < 0 || from_side.no >= edge_count {
            // "from side not calculated" — upstream warns and leaves the list
            // as it is, which then fails the stack-level check.
            return;
        }
        let side = from_side.no as usize;
        let compare_corner_1 = self.shape.corner_approx(side);
        let compare_corner_2 = if from_side.no == edge_count - 1 {
            self.shape.corner_approx(0)
        } else {
            self.shape.corner_approx(side + 1)
        };
        let mut from_point_dist = 0.0f64;
        let mut from_point_projection = None;
        if let Some(border_intersection) = from_side.border_intersection {
            let projection = border_intersection.projection_approx(&self.shape.border_line(side));
            from_point_dist = projection.distance_square(&compare_corner_1);
            from_point_projection = Some(projection);
            if from_point_dist >= compare_corner_1.distance_square(&compare_corner_2) {
                from_side = FromSide::raw(from_side.no, None);
                self.from_side = Some(from_side);
            }
        }

        // Find the first intersection between the side middle and corner 2.
        let mut curr = self.list_anchor;
        let mut prev = NONE;
        while curr != NONE {
            let entry = self.entries[curr as usize];
            if entry.edge_no > from_side.no {
                break;
            }
            if entry.edge_no == from_side.no {
                match (from_side.border_intersection, from_point_projection) {
                    (Some(_), Some(from_projection)) => {
                        let curr_projection = entry
                            .entry_approx
                            .projection_approx(&self.shape.border_line(side));
                        if curr_projection.distance_square(&compare_corner_1) >= from_point_dist
                            && curr_projection.distance_square(&from_projection)
                                <= curr_projection.distance_square(&compare_corner_1)
                        {
                            break;
                        }
                    }
                    _ => {
                        if entry.entry_approx.distance_square(&compare_corner_2)
                            <= entry.entry_approx.distance_square(&compare_corner_1)
                        {
                            break;
                        }
                    }
                }
            }
            prev = curr;
            curr = entry.next;
        }

        if curr != NONE && curr != self.list_anchor {
            let new_anchor = curr;
            while curr != NONE {
                prev = curr;
                curr = self.entries[prev as usize].next;
            }
            self.entries[prev as usize].next = self.list_anchor;
            curr = self.list_anchor;
            while curr != new_anchor {
                // `edge_no += edge_count` differentiates points before and
                // after the middle of the from-side, and keeps the order
                // monotone across the wrap.
                self.entries[curr as usize].edge_no += edge_count;
                prev = curr;
                curr = self.entries[prev as usize].next;
            }
            self.entries[prev as usize].next = NONE;
            self.list_anchor = new_anchor;
        }

        if self.list_anchor == NONE {
            return;
        }

        // Collapse runs: only the first and last intersection of a connected
        // set matter (`:475-497`).
        let mut prev = self.list_anchor;
        let mut prev_net_nos = self.nets_of(prev).clone();
        let mut curr = self.entries[prev as usize].next;
        let (mut curr_net_nos, mut next) = if curr != NONE {
            (self.nets_of(curr).clone(), self.entries[curr as usize].next)
        } else {
            (NetNos::default(), NONE)
        };
        let mut before_prev = NONE;
        while next != NONE {
            let next_net_nos = self.nets_of(next).clone();
            if geom::net_nos_equal(&prev_net_nos, &curr_net_nos)
                && geom::net_nos_equal(&curr_net_nos, &next_net_nos)
            {
                self.entries[prev as usize].next = next;
            } else {
                before_prev = prev;
                prev = curr;
                prev_net_nos = curr_net_nos;
            }
            curr_net_nos = next_net_nos;
            curr = next;
            next = self.entries[curr as usize].next;
        }

        // Strip own-net nodes at the end of the list (`:499-508`).
        if curr != NONE && geom::net_nos_equal(&curr_net_nos, &self.own_net_nos) {
            self.entries[prev as usize].next = NONE;
            if geom::net_nos_equal(&prev_net_nos, &self.own_net_nos) {
                if before_prev != NONE {
                    self.entries[before_prev as usize].next = NONE;
                } else {
                    self.list_anchor = NONE;
                }
            }
        }

        // ... and at the start (`:510-512`).
        if self.list_anchor != NONE
            && geom::net_nos_equal(self.nets_of(self.list_anchor), &self.own_net_nos)
        {
            self.list_anchor = self.entries[self.list_anchor as usize].next;
            if self.list_anchor != NONE
                && geom::net_nos_equal(self.nets_of(self.list_anchor), &self.own_net_nos)
            {
                self.list_anchor = self.entries[self.list_anchor as usize].next;
            }
        }
    }

    /// `calculate_stack_levels()` (`:515-609`) — the bracket-nesting calculus.
    ///
    /// Each maximal group of same-net entries becomes one substitute trace piece
    /// with a stack level equal to its nesting depth seen from the from-side.
    /// A crossing pattern (rather than a nesting one) fails.
    fn calculate_stack_levels(&mut self) -> bool {
        if self.list_anchor == NONE {
            return true;
        }
        let mut curr_entry = self.list_anchor;
        let mut curr_net_nos = self.nets_of(curr_entry).clone();
        let mut curr_level = if geom::net_nos_equal(&curr_net_nos, &self.own_net_nos) {
            // Own net is ignored when calculating the stack level.
            0
        } else {
            1
        };

        while curr_entry != NONE {
            if self.entries[curr_entry as usize].stack_level < 0 {
                self.trace_piece_count += 1;
                self.entries[curr_entry as usize].stack_level = curr_level;
                if curr_level > self.max_stack_level {
                    // Conformance note: the blame is recorded only when
                    // `max_stack_level` was *already* > 1 before the raise
                    // (`:530-540`).
                    if self.max_stack_level > 1 {
                        self.found_obstacle =
                            Some(self.traces[self.entries[curr_entry as usize].trace_ref as usize].id);
                    }
                    self.max_stack_level = curr_level;
                }
            }

            let mut check_entry = self.entries[curr_entry as usize].next;
            let mut index_of_next_foreign_set = 0usize;
            let mut index_of_last_occurrence_of_set = 0usize;
            let mut next_index = 0usize;
            let mut last_own_entry = NONE;
            let mut first_foreign_entry = NONE;
            let curr_stack_level = self.entries[curr_entry as usize].stack_level;

            while check_entry != NONE {
                next_index += 1;
                if geom::net_nos_equal(self.nets_of(check_entry), &curr_net_nos) {
                    index_of_last_occurrence_of_set = next_index;
                    last_own_entry = check_entry;
                    self.entries[check_entry as usize].stack_level = curr_stack_level;
                } else if index_of_next_foreign_set == 0 {
                    index_of_next_foreign_set = next_index;
                    first_foreign_entry = check_entry;
                }
                check_entry = self.entries[check_entry as usize].next;
            }

            if next_index == 0 {
                curr_entry = NONE;
                continue;
            }

            let next_entry;
            if index_of_next_foreign_set != 0
                && index_of_next_foreign_set < index_of_last_occurrence_of_set
            {
                // Interleaved ⇒ nested: raise the level.
                next_entry = first_foreign_entry;
                if self.entries[next_entry as usize].stack_level >= 0 {
                    // Stack property fails: a crossing, not a nesting.
                    return false;
                }
                curr_level += 1;
            } else if index_of_last_occurrence_of_set != 0 {
                next_entry = last_own_entry;
            } else {
                next_entry = first_foreign_entry;
                if self.entries[next_entry as usize].stack_level >= 0 {
                    // Popping a bracket.
                    curr_level -= 1;
                    if self.entries[next_entry as usize].stack_level != curr_level {
                        return false;
                    }
                }
            }
            curr_net_nos = self.nets_of(next_entry).clone();
            // Splice out everything between `curr_entry` and `next_entry`.
            self.entries[curr_entry as usize].next = next_entry;
            curr_entry = next_entry;
        }

        if curr_level != 1 {
            // "curr_level inconsistent" (`:604`).
            return false;
        }
        true
    }

    /// `pop_piece()` (`:615-667`): remove the first maximal run of entries at
    /// `max_stack_level` with equal nets, then recompute the maximum over the
    /// remainder. Own-net runs are skipped — they occur only at level 0 — so
    /// pieces come out **outermost first**.
    fn pop_piece(&mut self) -> Option<(u32, u32)> {
        loop {
            if self.list_anchor == NONE {
                return None;
            }
            let mut first = self.list_anchor;
            let mut prev_first = NONE;
            while first != NONE && self.entries[first as usize].stack_level != self.max_stack_level
            {
                prev_first = first;
                first = self.entries[first as usize].next;
            }
            if first == NONE {
                // "max_stack_level not found".
                return None;
            }
            let first_nets = self.nets_of(first).clone();
            let mut last = first;
            let mut after_last = self.entries[first as usize].next;
            while after_last != NONE
                && self.entries[after_last as usize].stack_level == self.max_stack_level
                && geom::net_nos_equal(self.nets_of(after_last), &first_nets)
            {
                last = after_last;
                after_last = self.entries[last as usize].next;
            }

            if prev_first != NONE {
                self.entries[prev_first as usize].next = after_last;
            } else {
                self.list_anchor = after_last;
            }

            self.max_stack_level = 0;
            let mut curr = self.list_anchor;
            while curr != NONE {
                self.max_stack_level = self.max_stack_level.max(self.entries[curr as usize].stack_level);
                curr = self.entries[curr as usize].next;
            }
            self.trace_piece_count -= 1;
            if geom::net_nos_equal(&first_nets, &self.own_net_nos) {
                // Own net is ignored and may occur only at the lowest level.
                continue;
            }
            return Some((first, last));
        }
    }

    /// `next_substitute_trace_piece()` (`:176-221`): the popped entry pair as a
    /// polyline hugging the offset border between the two entry lines.
    pub fn next_substitute_trace_piece(&mut self, board: &Board) -> Option<SubstitutePiece> {
        loop {
            let (first, last) = self.pop_piece()?;
            let info = self.traces[self.entries[first as usize].trace_ref as usize].clone();
            let offset_shape = offset_shape_for_trace(
                board,
                &self.shape,
                info.half_width,
                info.cl_class,
                self.layer,
                self.cl_class,
            );
            let edge_count = self.shape.border_line_count();
            if edge_count == 0 || offset_shape.border_line_count() != edge_count {
                continue;
            }
            let edge_diff = self.entries[last as usize].edge_no - self.entries[first as usize].edge_no;
            if edge_diff < 0 {
                continue;
            }
            let mut piece_lines = Vec::with_capacity(edge_diff as usize + 3);
            piece_lines.push(self.entries[first as usize].line);
            let mut curr_edge_no = (self.entries[first as usize].edge_no as usize) % edge_count;
            for _ in 0..=edge_diff {
                piece_lines.push(offset_shape.border_line(curr_edge_no));
                curr_edge_no = if curr_edge_no == edge_count - 1 {
                    0
                } else {
                    curr_edge_no + 1
                };
            }
            piece_lines.push(self.entries[last as usize].line);
            let piece_polyline = Polyline::new_from_lines(&piece_lines);
            if piece_polyline.is_empty() {
                // No valid trace piece; go on to the next one.
                continue;
            }
            return Some(SubstitutePiece {
                polyline: piece_polyline,
                layer: self.layer,
                half_width: info.half_width,
                nets: info.nets,
                cl_class: info.cl_class,
            });
        }
    }

    /// `cutout_traces(item_list)` (`:227-234`): cut the shove shape out of every
    /// foreign obstacle trace. **Mutates the board.**
    pub fn cutout_traces(&self, board: &mut Board, item_list: &[ItemId]) {
        for &id in item_list {
            let Some(item) = board.store().get(id) else {
                continue;
            };
            if !matches!(item.kind, ItemKind::Trace(_)) || item.nets.shares_any(&self.own_net_nos) {
                continue;
            }
            cutout_trace(board, id, &self.shape, self.cl_class);
        }
    }

    /// The shove shape this instance was built for.
    pub fn shape(&self) -> &Simplex {
        &self.shape
    }
}

/// `ShapeTraceEntries.cutout_trace(trace, shape, cl_class)` (`:48-79`).
///
/// Upstream's `fast_cutout_trace` (`:84-105`) is the same operation with the
/// search-tree leaves reused instead of rebuilt; here remove + insert produces
/// the identical index (`fr-board` module docs, "what this wave left out"), so
/// the fast path collapses into the general one.
pub fn cutout_trace(board: &mut Board, id: ItemId, shape: &Simplex, cl_class: i32) {
    let Some(item) = board.store().get(id) else {
        return;
    };
    let ItemKind::Trace(trace) = &item.kind else {
        return;
    };
    let (layer, half_width, polyline) = (trace.layer, trace.half_width, trace.polyline.clone());
    let (nets, item_cl_class) = (item.nets.clone(), item.clearance_class);
    let offset_shape = offset_shape_for_trace(
        board,
        shape,
        half_width,
        item_cl_class,
        layer,
        cl_class,
    );
    if offset_shape.is_empty() {
        return;
    }
    let pieces = offset_shape.cutout_polyline(&polyline);
    if pieces.len() == 1 && pieces[0] == polyline {
        // Nothing cut off.
        return;
    }
    board.remove_item(id);
    for piece in pieces {
        board.insert_trace_without_cleaning(
            piece,
            layer,
            half_width,
            nets.clone(),
            item_cl_class,
            FixedState::Unfixed,
        );
    }
}

/// `RoutingBoard.contains_trace_tails(items, own_net_nos)`.
pub fn contains_trace_tails(board: &Board, item_list: &[ItemId], own_net_nos: &NetNos) -> bool {
    let mut contacts = Vec::new();
    for &id in item_list {
        let Some(item) = board.store().get(id) else {
            continue;
        };
        if item.nets.shares_any(own_net_nos) {
            continue;
        }
        let ItemKind::Trace(trace) = &item.kind else {
            continue;
        };
        for corner in [trace.first_corner(), trace.last_corner()] {
            board.normal_contacts_at(id, &corner, &mut contacts);
            if contacts.is_empty() {
                return true;
            }
        }
    }
    false
}

/// `Point` is only used through the trace corners; this keeps the import honest.
const _: fn(&Point) -> FloatPoint = geom::point_to_float;

#[cfg(test)]
impl ShapeTraceEntries {
    /// Appends a synthetic entry point on `edge_no`, so the bracket calculus can
    /// be exercised without a board.
    fn push_test_entry(&mut self, nets: &[i32], edge_no: usize) {
        let count = self.shape.border_line_count();
        let entry_approx = self
            .shape
            .corner_approx(edge_no)
            .middle_point(&self.shape.corner_approx((edge_no + 1) % count));
        let line = self.shape.border_line(edge_no);
        let nets = NetNos::from_slice(nets);
        let trace_ref = match self.traces.iter().position(|info| info.nets == nets) {
            Some(index) => index as u32,
            None => {
                self.traces.push(TraceInfo {
                    id: ItemId::UNASSIGNED,
                    nets,
                    half_width: 10,
                    cl_class: 1,
                });
                (self.traces.len() - 1) as u32
            }
        };
        self.insert_entry_point(trace_ref, line, edge_no as i32, entry_approx);
    }

    /// `store_items`' tail: resort then the bracket calculus.
    fn run_stack_calculus(&mut self) -> bool {
        self.search_from_side();
        self.resort();
        self.calculate_stack_levels()
    }

    /// The net sets of the pieces, outermost first.
    fn popped_piece_nets(&mut self) -> Vec<Vec<i32>> {
        let mut result = Vec::new();
        while let Some((first, _last)) = self.pop_piece() {
            result.push(self.nets_of(first).as_slice().to_vec());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_geometry::planar::IntPoint;

    /// An eight-sided shape, so seven edge slots are free above the from-side.
    fn octagon() -> Simplex {
        Simplex::from_convex_polygon(&[
            IntPoint::new(0, 0),
            IntPoint::new(100, 0),
            IntPoint::new(150, 50),
            IntPoint::new(150, 100),
            IntPoint::new(100, 150),
            IntPoint::new(0, 150),
            IntPoint::new(-50, 100),
            IntPoint::new(-50, 50),
        ])
    }

    /// Entries in `sequence` laid out one per edge, starting at edge 1 so the
    /// from-side (edge 0) stays clear and `resort` does not rotate.
    fn entries_for(sequence: &[i32]) -> ShapeTraceEntries {
        let shape = octagon();
        assert!(
            shape.border_line_count() > sequence.len(),
            "the fixture shape has an edge per entry plus the from-side"
        );
        let mut entries = ShapeTraceEntries::new(
            shape,
            0,
            NetNos::of(99),
            1,
            Some(FromSide::raw(0, None)),
        );
        for (index, net) in sequence.iter().enumerate() {
            entries.push_test_entry(&[*net], index + 1);
        }
        entries
    }

    #[test]
    fn two_disjoint_nets_are_two_pieces_at_one_level() {
        // A A B B — properly nested, nothing inside anything.
        let mut entries = entries_for(&[1, 1, 2, 2]);
        assert!(entries.run_stack_calculus());
        assert_eq!(entries.stack_depth(), 1);
        assert_eq!(entries.substitute_trace_count(), 2);
    }

    #[test]
    fn a_nested_net_raises_the_stack_level_past_the_gate() {
        // A B B A — B is *behind* A seen from the from-side, so it nests.
        // Every check path refuses `stack_depth() > 1`.
        let mut entries = entries_for(&[1, 2, 2, 1]);
        assert!(entries.run_stack_calculus());
        assert_eq!(entries.stack_depth(), 2, "one net is nested inside another");
    }

    #[test]
    fn a_crossing_pattern_fails_the_stack_property() {
        // A B A B — a genuine weave, not a nesting. Unshovable.
        let mut entries = entries_for(&[1, 2, 1, 2]);
        assert!(
            !entries.run_stack_calculus(),
            "interleaved nets must fail the bracket calculus"
        );
    }

    #[test]
    fn a_run_of_one_net_collapses_to_its_first_and_last_intersection() {
        // A A A A — only the outermost pair matters (`resort`, `:475-497`).
        let mut entries = entries_for(&[1, 1, 1, 1]);
        assert!(entries.run_stack_calculus());
        assert_eq!(entries.substitute_trace_count(), 1);
        let mut count = 0;
        let mut node = entries.list_anchor;
        while node != NONE {
            count += 1;
            node = entries.entries[node as usize].next;
        }
        assert_eq!(count, 2, "the interior intersections are dropped");
    }

    #[test]
    fn pieces_pop_outermost_first() {
        // A B B A: B sits at level 2, A at level 1, so B is produced first —
        // upstream clears the deepest obstacle before the shallower one.
        let mut entries = entries_for(&[1, 2, 2, 1]);
        assert!(entries.run_stack_calculus());
        assert_eq!(entries.popped_piece_nets(), vec![vec![2], vec![1]]);
    }

    #[test]
    fn own_net_entries_are_stripped_from_both_ends() {
        // The own net is 99; it may touch the shape without being shoved.
        let mut entries = entries_for(&[99, 1, 1, 99]);
        assert!(entries.run_stack_calculus());
        assert_eq!(entries.substitute_trace_count(), 1);
        assert_eq!(entries.popped_piece_nets(), vec![vec![1]]);
    }

    #[test]
    fn an_empty_entry_list_is_trivially_shovable() {
        let mut entries = entries_for(&[]);
        assert!(entries.run_stack_calculus());
        assert_eq!(entries.stack_depth(), 0);
        assert_eq!(entries.substitute_trace_count(), 0);
    }
}
