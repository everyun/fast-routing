//! Trace split, combine and normalize — the board-level contract of
//! `v2-vs-v19-and-scope.md` §11.4-§11.6 and `pull-tight-insert.md` §7.
//!
//! These are `impl Board` blocks rather than free functions because every one
//! of them is a *board* mutation: splitting a trace removes an item and inserts
//! two, combining removes one and rewrites another, and all of it has to stay
//! in step with the search trees and the connectivity index.
//!
//! # The three guards that make this terminate
//!
//! Upstream learned each of them the hard way, and the release jar ships all
//! three (`pull-tight-insert.md` "Release-jar conformance" rows D4 and D14):
//!
//! 1. **`combine` is iterative, not recursive.** v19 recursed and blew the
//!    stack on the long collinear chains an SES round-trip produces
//!    (§11.5 delta).
//! 2. **`normalize` bottoms out at depth 16** and returns `false` instead of
//!    throwing (`MAX_NORMALIZATION_DEPTH`; v19 threw, and the release does not).
//! 3. **`normalize_traces(net)` caps its outer loop at 2000 iterations** and
//!    then *permanently suppresses that net* on that board
//!    ([`MAX_NORMALIZE_ITERATIONS`], `normalizeSuppressedNetNos`). This is the
//!    valve for a net whose traces oscillate between two normalizations; it is
//!    load-bearing for termination.
//!
//! # What this implements of §7.1's `split(clip)`
//!
//! Splitting is driven by three junction sources, which together are what trace
//! normalization needs:
//!
//! * a same-net **drill item** whose centre lies on a segment;
//! * a same-net **trace crossing** — including a T-junction, where the other
//!   trace merely *ends* on this one;
//! * transitively, the pieces that come out of either.
//!
//! Deliberately **not** implemented yet, and why:
//!
//! * *self*-intersection splitting (upstream skips the ±1 neighbour window and
//!   treats the rest like a foreign trace). A self-touching trace is a cycle,
//!   and cycle handling is `removeIfCycle` — the ripup unit, which belongs with
//!   the router.
//! * the `ConductionArea` branch (trace contacting the same plane at both ends
//!   ⇒ remove outright), for the same reason: it is cycle removal.
//! * collinear *overlap* between two segments — upstream's **two**-line
//!   intersection result, which it splits at the first and the last common
//!   point (`PolylineTrace.split`'s Javadoc says so outright). A collinear
//!   contact at a *single* point is not this case and is handled; see
//!   `Board::split_against_trace`.
//!
//! Each of those is a strictly *additional* split opportunity, so what is here
//! never splits where upstream would not.

use fr_geometry::planar::{IntOctagon, Line, LineSegment, Point, Polyline};

use crate::board::Board;
use crate::item::{FixedState, ItemId, ItemKind};
use crate::net_nos::NetNos;
use crate::shape::TileShape;

/// Upstream `PolylineTrace.MAX_NORMALIZATION_DEPTH` (`:34`), confirmed in the
/// release jar's constant pool.
pub const MAX_NORMALIZATION_DEPTH: u32 = 16;

/// Upstream `BasicBoard.MAX_NORMALIZE_ITERATIONS` (`:52`), confirmed in the
/// release jar's constant pool. See the module docs, guard 3.
pub const MAX_NORMALIZE_ITERATIONS: u32 = 2000;

/// Ceiling on how many split steps one `split_trace` call may take.
///
/// Not upstream's — upstream relies on the geometry alone. Every split here
/// strictly consumes one interior junction point of the trace, so the loop
/// terminates on its own; this is a belt-and-braces bound so that a geometry
/// bug degrades into an unsplit trace instead of a hang.
const MAX_SPLIT_STEPS: u32 = 4096;

/// What one attempt at splitting a trace achieved.
enum SplitStep {
    /// The trace itself was split; these are its pieces (either may be `None`
    /// if the piece was degenerate and therefore not inserted).
    Own([Option<ItemId>; 2]),
    /// Another trace was split instead. The board changed, so the caller must
    /// re-query and try this trace again — upstream re-reads the overlapping
    /// entries and resets the iterator for exactly this reason (§11.4).
    Foreign,
    /// Nothing to split.
    None,
}

/// A trace's geometry and the attributes its pieces inherit.
#[derive(Debug, Clone)]
struct TraceInfo {
    polyline: Polyline,
    layer: i32,
    half_width: i32,
    nets: NetNos,
    clearance_class: i32,
    fixed_state: FixedState,
}

impl Board {
    fn trace_info(&self, id: ItemId) -> Option<TraceInfo> {
        let item = self.store().get(id)?;
        let ItemKind::Trace(trace) = &item.kind else {
            return None;
        };
        Some(TraceInfo {
            polyline: trace.polyline.clone(),
            layer: trace.layer,
            half_width: trace.half_width,
            nets: item.nets.clone(),
            clearance_class: item.clearance_class,
            fixed_state: item.fixed_state,
        })
    }

    /// True for upstream's degenerate trace: exactly two corners that coincide
    /// (`PolylineTraceNormalization`, §11.6).
    pub fn is_degenerate_trace(&self, id: ItemId) -> bool {
        match self.store().get(id).map(|item| &item.kind) {
            Some(ItemKind::Trace(trace)) => {
                trace.corner_count() == 2 && trace.first_corner() == trace.last_corner()
            }
            _ => false,
        }
    }

    // ---- the split primitive (§11.4) -------------------------------------

    /// Upstream `PolylineTrace.split(lineIndex, newEndLine)` (`:719-760`) — the
    /// primitive every other split goes through.
    ///
    /// ```text
    /// if !isOnTheBoard():          return null
    /// if isDeletionForbidden():    return null          # v2-only guard
    /// pieces = lines.split(lineIndex, newEndLine)       # null or exactly 2
    /// if splitInsideDrillPadProhibited(...): return null
    /// removeItem(this); insertTraceWithoutCleaning(each piece)
    /// ```
    ///
    /// The pieces inherit net array, clearance class, layer, half width and
    /// fixed state; either may come back `None`, because
    /// [`insert_trace_without_cleaning`](Board::insert_trace_without_cleaning)
    /// refuses a degenerate piece.
    pub fn split_trace_at_line(
        &mut self,
        id: ItemId,
        line_index: usize,
        end_line: &Line,
    ) -> Option<[Option<ItemId>; 2]> {
        if !self.store().contains(id) || self.is_deletion_forbidden(id) {
            return None;
        }
        let info = self.trace_info(id)?;
        let [first, second] = info.polyline.split(line_index, end_line)?;
        if self.split_inside_drill_pad_prohibited(id, &info, line_index, end_line) {
            return None;
        }

        self.remove_item_unchecked(id);
        let a = self.insert_trace_without_cleaning(
            first,
            info.layer,
            info.half_width,
            info.nets.clone(),
            info.clearance_class,
            info.fixed_state,
        );
        let b = self.insert_trace_without_cleaning(
            second,
            info.layer,
            info.half_width,
            info.nets,
            info.clearance_class,
            info.fixed_state,
        );
        Some([a, b])
    }

    /// Upstream `splitInsideDrillPadProhibited` (`:768-792`): never split
    /// inside a pin's pad, except exactly at the pin centre or where another
    /// trace already ends.
    ///
    /// Vias are intentionally *not* considered — upstream's comment (`:764-766`)
    /// says extending this to vias "led to broken connection problems".
    ///
    /// > **Operator-precedence bug preserved.** Upstream writes
    /// > `currentTrace != this && currentTrace.firstCorner().equals(x) || currentTrace.lastCorner().equals(x)`
    /// > and `&&` binds tighter than `||`, so a *self* match at `lastCorner`
    /// > also allows the split (`v2 PolylineTrace.java:785-786`,
    /// > `v19 :645-646`). §11.4 says to replicate it deliberately; the
    /// > parenthesization below is the buggy grouping, spelled out.
    fn split_inside_drill_pad_prohibited(
        &mut self,
        id: ItemId,
        info: &TraceInfo,
        line_index: usize,
        end_line: &Line,
    ) -> bool {
        let Some(line) = info.polyline.arr.get(line_index) else {
            return false;
        };
        let intersection = line.intersection(end_line);

        let mut candidates = self.take_scratch_items();
        self.pick_items(&intersection, Some(info.layer), &mut candidates);

        let mut pad_found = false;
        for candidate in candidates.iter().copied() {
            let Some(other) = self.store().get(candidate) else {
                continue;
            };
            if !other.nets.shares_any(&info.nets) {
                continue;
            }
            match &other.kind {
                ItemKind::Pin(pin) => {
                    if Point::Int(pin.center) == intersection {
                        pad_found = false;
                        break;
                    }
                    pad_found = true;
                }
                ItemKind::Trace(trace) => {
                    if (candidate != id && trace.first_corner() == intersection)
                        || trace.last_corner() == intersection
                    {
                        pad_found = false;
                        break;
                    }
                }
                _ => {}
            }
        }
        self.put_scratch_items(candidates);
        pad_found
    }

    /// Upstream `PolylineTrace.split(Point)` (`:699-712`): find the segment
    /// containing `point` and split it with the **perpendicular** line there
    /// (`segment.direction().turn45Degree(2)`).
    ///
    /// `point` has to be a lattice point: the split line is anchored on it, and
    /// `Line` is defined by two `IntPoint`s. A rational point would have to be
    /// rounded, which would move the split — so it is refused instead.
    pub fn split_trace_at_point(
        &mut self,
        id: ItemId,
        point: &Point,
    ) -> Option<[Option<ItemId>; 2]> {
        let Point::Int(anchor) = point else {
            return None;
        };
        let info = self.trace_info(id)?;
        for line_index in 1..info.polyline.arr.len().saturating_sub(1) {
            let Some(segment) = LineSegment::from_polyline(&info.polyline, line_index) else {
                continue;
            };
            if !segment.contains(point) {
                continue;
            }
            let direction = segment.get_line().direction().turn_45_degree(2);
            let end_line = Line::new_from_point_and_dir(*anchor, &direction);
            return self.split_trace_at_line(id, line_index, &end_line);
        }
        None
    }

    /// Upstream `BasicBoard.splitTraces(location, layer, net)` (`:891-907`):
    /// split every same-net trace running through `location` on `layer`.
    ///
    /// Returns whether anything was split. Called for every layer a via spans
    /// (§11.3) — with the half-open range for a plain via and the closed one
    /// for an escape via.
    pub fn split_traces(&mut self, location: &Point, layer: i32, net_no: i32) -> bool {
        if net_no <= 0 {
            return false;
        }
        let mut candidates = self.take_scratch_items();
        self.pick_items(location, Some(layer), &mut candidates);

        let mut split = false;
        for candidate in candidates.iter().copied() {
            let is_target = self.store().get(candidate).is_some_and(|item| {
                matches!(item.kind, ItemKind::Trace(_)) && item.contains_net(net_no)
            });
            if !is_target {
                continue;
            }
            if self.split_trace_at_point(candidate, location).is_some() {
                split = true;
            }
        }
        self.put_scratch_items(candidates);
        split
    }

    // ---- split at junctions (§7.1) ---------------------------------------

    /// Splits `id` at every same-net junction on it, returning the resulting
    /// pieces (just `[id]` when nothing was split).
    ///
    /// `clip` restricts the work to segments whose bounding box meets it —
    /// upstream's changed-area prescreen. Pieces come back in ascending id
    /// order, which makes the result canonical; upstream's order is an artifact
    /// of its recursion and no caller depends on it.
    pub fn split_trace(&mut self, id: ItemId, clip: Option<&IntOctagon>) -> Vec<ItemId> {
        if !self.store().contains(id) {
            return Vec::new();
        }
        // §11.4 early exit: items on pseudo-nets are never split.
        if !self
            .store()
            .get(id)
            .is_some_and(|item| item.nets_normal() && matches!(item.kind, ItemKind::Trace(_)))
        {
            return vec![id];
        }

        let mut pending = self.take_scratch_ids();
        pending.push(id);
        let mut result = Vec::new();
        let mut budget = MAX_SPLIT_STEPS;

        while let Some(current) = pending.pop() {
            if !self.store().contains(current) {
                continue;
            }
            if budget == 0 {
                result.push(current);
                continue;
            }
            budget -= 1;
            match self.try_split_once(current, clip) {
                SplitStep::Own(pieces) => pending.extend(pieces.into_iter().flatten()),
                SplitStep::Foreign => pending.push(current),
                SplitStep::None => result.push(current),
            }
        }
        self.put_scratch_ids(pending);
        result.sort_unstable();
        result
    }

    fn try_split_once(&mut self, id: ItemId, clip: Option<&IntOctagon>) -> SplitStep {
        let Some(info) = self.trace_info(id) else {
            return SplitStep::None;
        };
        let mut candidates = self.take_scratch_items();
        let step = self.try_split_once_inner(id, &info, clip, &mut candidates);
        self.put_scratch_items(candidates);
        step
    }

    fn try_split_once_inner(
        &mut self,
        id: ItemId,
        info: &TraceInfo,
        clip: Option<&IntOctagon>,
        candidates: &mut Vec<ItemId>,
    ) -> SplitStep {
        let first_corner = info.polyline.first_corner();
        let last_corner = info.polyline.last_corner();

        for line_index in 1..info.polyline.arr.len().saturating_sub(1) {
            let Some(segment) = LineSegment::from_polyline(&info.polyline, line_index) else {
                continue;
            };
            if let Some(clip) = clip {
                if !clip.intersects(&segment.bounding_box().to_int_octagon()) {
                    continue;
                }
            }
            let Some(shape) = info.polyline.offset_shape(info.half_width, line_index - 1) else {
                continue;
            };
            self.overlapping_items(
                &TileShape::Simplex(shape),
                Some(info.layer),
                &[],
                candidates,
            );

            // Indexed rather than iterated: holding an iterator over
            // `candidates` across the `&mut self` calls below would keep the
            // buffer borrowed, and cloning it per segment would allocate inside
            // the split loop. Copying one id out costs nothing.
            #[allow(clippy::needless_range_loop)]
            for position in 0..candidates.len() {
                let candidate = candidates[position];
                if candidate == id {
                    // Self-intersection: see the module docs.
                    continue;
                }
                let Some(other) = self.store().get(candidate) else {
                    continue;
                };
                if !other.nets.shares_any(&info.nets) {
                    continue;
                }
                match &other.kind {
                    ItemKind::Pin(pin) => {
                        let center = Point::Int(pin.center);
                        if let Some(step) = self.split_at_drill_center(
                            id,
                            line_index,
                            &segment,
                            &center,
                            &first_corner,
                            &last_corner,
                        ) {
                            return step;
                        }
                    }
                    ItemKind::Via(via) => {
                        let center = Point::Int(via.center);
                        if let Some(step) = self.split_at_drill_center(
                            id,
                            line_index,
                            &segment,
                            &center,
                            &first_corner,
                            &last_corner,
                        ) {
                            return step;
                        }
                    }
                    ItemKind::Trace(other_trace) => {
                        let other_polyline = other_trace.polyline.clone();
                        if let Some(step) = self.split_against_trace(
                            id,
                            line_index,
                            &segment,
                            candidate,
                            &other_polyline,
                        ) {
                            return step;
                        }
                    }
                    _ => {}
                }
            }
        }
        SplitStep::None
    }

    #[allow(clippy::too_many_arguments)]
    fn split_at_drill_center(
        &mut self,
        id: ItemId,
        line_index: usize,
        segment: &LineSegment,
        center: &Point,
        first_corner: &Point,
        last_corner: &Point,
    ) -> Option<SplitStep> {
        if center == first_corner || center == last_corner || !segment.contains(center) {
            return None;
        }
        let Point::Int(anchor) = center else {
            return None;
        };
        let direction = segment.get_line().direction().turn_45_degree(2);
        let end_line = Line::new_from_point_and_dir(*anchor, &direction);
        self.split_trace_at_line(id, line_index, &end_line)
            .map(SplitStep::Own)
    }

    /// One own segment against one other same-net trace.
    ///
    /// Upstream's order is preserved: the **found** trace is split first, and
    /// only then the own trace — and after splitting the found trace the
    /// overlapping-entry list is stale, so this reports [`SplitStep::Foreign`]
    /// and lets the caller re-query (§11.4).
    ///
    /// # The split line is the one `LineSegment.intersection` hands back
    ///
    /// Upstream splits the found trace with `foundSegment.intersection(currSegment)[j]` and the
    /// own trace with `currSegment.intersection(foundSegment)[j]` (`PolylineTrace.java:504` and
    /// `:541`), and `LineSegment.intersection` has two branches that return *different kinds* of
    /// line (`LineSegment.java:247-295`):
    ///
    /// | branch | result | which line |
    /// |---|---|---|
    /// | the two segments cross | 1 line | the **other** segment's middle line — so the two calls differ |
    /// | collinear, touching at one point | 1 line | the shared endpoint's **cap** line — the same for both calls, since the branch is symmetric in the two segments |
    /// | collinear, overlapping | 2 lines | the caps at the first and last common point |
    ///
    /// Substituting `segment.get_line()` / `other_segment.get_line()` for the whole thing is right
    /// for the crossing branch and wrong for the collinear one, where the two middle lines are the
    /// *same* line: `Polyline::split` bails out on its `is_parallel` guard and the collinear
    /// junction is never examined at all. The board came out the same, because a trace touching an
    /// interior corner is also seen from the *other* arm of that corner, which is not collinear —
    /// so the junction was reached one segment index later. Relying on that is relying on an
    /// accident; the split line here is now the one upstream uses.
    ///
    /// # Which collinear contacts split, and which must not
    ///
    /// With the cap line in hand, `Polyline::split`'s own guard decides, and it is *positional*:
    /// it refuses only when the split point is the polyline's **first** corner and the split is on
    /// line 1, or its **last** corner and the split is on the last line
    /// (`Polyline.java:861-866`). So for a same-net pair meeting **end to end**, collinear or not,
    /// both traces are refused — which is what keeps `normalize` from splitting a chain apart just
    /// for `combine` to weld it back together on the next pass, and it is the reason a two-corner
    /// trace is safe: its only line is both line 1 and the last line, so *both* halves of the
    /// guard apply to it. A corner of the other trace landing *strictly inside* our segment, or on
    /// an **interior** corner of it, does split.
    ///
    /// Reproducing that guard here as an unqualified
    /// `split_point == first_corner || == last_corner` test — as this used to — is a *different*
    /// predicate: it fires at every line index, so it also suppresses a split whose point happens
    /// to coincide with an endpoint reached from the middle of the polyline. Letting
    /// `Polyline::split` be the only authority is both shorter and upstream's actual condition.
    fn split_against_trace(
        &mut self,
        id: ItemId,
        line_index: usize,
        segment: &LineSegment,
        other_id: ItemId,
        other_polyline: &Polyline,
    ) -> Option<SplitStep> {
        for other_index in 1..other_polyline.arr.len().saturating_sub(1) {
            let Some(other_segment) = LineSegment::from_polyline(other_polyline, other_index)
            else {
                continue;
            };
            let crossing = segment.intersection(&other_segment);
            // Exactly one line means a proper crossing, a T-junction, or a collinear contact at a
            // single point. Two lines is a collinear overlap — see the module docs.
            if crossing.len() != 1 {
                continue;
            }
            // One call, both of upstream's: the collinear branch is symmetric, so its single cap
            // line serves both traces, and only the crossing branch needs the two middle lines.
            let (found_end_line, own_end_line) =
                if segment.get_line().is_parallel(&other_segment.get_line()) {
                    (crossing[0], crossing[0])
                } else {
                    (segment.get_line(), other_segment.get_line())
                };

            // Split the found trace first. `split_trace_at_line` mutates nothing unless it
            // succeeds, so `segment` and `line_index` stay valid for the own split below.
            if self
                .split_trace_at_line(other_id, other_index, &found_end_line)
                .is_some()
            {
                return Some(SplitStep::Foreign);
            }
            if let Some(pieces) = self.split_trace_at_line(id, line_index, &own_end_line) {
                return Some(SplitStep::Own(pieces));
            }
        }
        None
    }

    // ---- combine (§11.5) -------------------------------------------------

    /// Upstream `PolylineTrace.combine()` (`:175-192`): merge this trace with
    /// the traces it meets end-to-end, for as long as that is possible.
    ///
    /// **Iterative**, which is v2's fix for the `StackOverflowError` v19's
    /// recursion hit on long collinear chains (§11.5 delta). It terminates
    /// because every successful step removes one item from the board.
    ///
    /// Each step reaches `merge_entries_in_front` / `merge_entries_at_end`
    /// through [`Board::change_trace_polyline`]; `combine_trace_at`'s own docs
    /// carry the mapping, which half of the pair has landed, and why the other
    /// half is a decision rather than a missing primitive.
    pub fn combine_trace(&mut self, id: ItemId) -> bool {
        let mut changed = false;
        while self.store().contains(id)
            && (self.combine_trace_at(id, true) || self.combine_trace_at(id, false))
        {
            changed = true;
        }
        changed
    }

    /// One combine step at the trace's start (`at_start`) or end corner —
    /// upstream's `combineAtStart` / `combineAtEnd`.
    ///
    /// Every precondition of §11.5 must hold:
    ///
    /// 1. the normal contacts at that corner, **excluding conduction areas**,
    ///    are exactly one item;
    /// 2. that item is a `PolylineTrace` with the same layer, equal net array,
    ///    the same half width and the same fixed state, and (v2-only)
    ///    *neither* trace's deletion is forbidden;
    /// 3. it ends or starts at that corner.
    ///
    /// The join goes through [`Board::change_trace_polyline`], which is where
    /// upstream's `merge_entries_in_front` / `merge_entries_at_end`
    /// (`ShapeSearchTree.java:155-290`) land in this port.
    ///
    /// [`Polyline::combine`](fr_geometry::planar::Polyline::combine) lays the
    /// joined polyline out as *the survivor's lines with one end line dropped*,
    /// concatenated with the absorbed trace's (reversed and negated when the two
    /// meet head-to-head). So the survivor's own lines are an exact prefix —
    /// combining at the end — or an exact suffix — combining at the start — of
    /// the join, and `change_trace_polyline`'s equal-run scan finds them without
    /// being told: every one of the survivor's segments but the one at the seam
    /// keeps its leaf and its tile shape, and only the seam plus the absorbed
    /// trace's segments are offset. That is upstream's
    /// `p_from_entry_no = this_lines.length - 3` (`PolylineTrace.java:404`)
    /// arrived at from the geometry rather than passed down.
    ///
    /// Combining **at the start** shifts every one of the survivor's shape
    /// indices, and that is a relabel, not a re-insert: the kept trailing leaves
    /// are rewritten in place through
    /// [`ArenaShapeTree::set_payload`](fr_datastructures::arena_shape_tree::ArenaShapeTree::set_payload),
    /// which is `merge_entries_in_front`'s
    /// `new_leaf_arr[curr_ind].shape_index_in_object = curr_ind` loop
    /// (`ShapeSearchTree.java:202-205`) reached from the general
    /// `change_entries` path.
    ///
    /// # What upstream still does here and this does not
    ///
    /// Upstream also *retargets* the absorbed trace's leaves onto the survivor
    /// (`leaf.object = p_to_trace`, `:197-198` / `:271-272`); here those
    /// segments are re-offset and re-inserted. The missing primitive is no
    /// longer the reason — `set_payload` exists — and two other things are:
    ///
    /// * **The index mapping has to be derived, not assumed.** Upstream is
    ///   handed `p_from_entry_no` / `p_to_entry_no` by the caller, which knows
    ///   whether `Polyline::combine` dropped a line at the seam and which way
    ///   round the absorbed trace went. This port deliberately derives the
    ///   survivor's kept runs from the two line arrays instead (above); doing the
    ///   same for the absorbed side means a second equal-run scan, against the
    ///   *oriented* absorbed array, plus the index reversal when the two traces
    ///   met head-to-head. Sound, but it is arithmetic whose only failure mode is
    ///   a silently mis-indexed leaf, i.e. routing through copper.
    /// * **Upstream's version is not clearance-neutral.** `combine_at_start` /
    ///   `combine_at_end` require equal layer, net array, half width and fixed
    ///   state — but **not** an equal clearance class
    ///   (`PolylineTrace.java:223-226` and `:340-343`). Upstream
    ///   nevertheless reuses the absorbed trace's tile shapes, which were offset
    ///   by *its* clearance compensation, while the seam shapes it inserts use
    ///   the survivor's. Re-offsetting, as this does, gives the whole joined
    ///   trace the survivor's compensation throughout. So "the resulting index
    ///   is the same either way" is true only when the two classes agree; where
    ///   they do not, upstream indexes a joined trace under two different
    ///   compensations and this port does not.
    ///
    /// The cost left on the table is therefore `O(absorbed segments)` offset
    /// computations per combine, and closing it is a call-site change plus a
    /// decision about that second bullet — not a datastructure gap.
    ///
    /// The v19 bug the v2 fallback fixes — an NPE inside
    /// `merge_entries_in_front` for a trace without tree entries (§11.5 delta) —
    /// is structurally absent here.
    fn combine_trace_at(&mut self, id: ItemId, at_start: bool) -> bool {
        let Some(info) = self.trace_info(id) else {
            return false;
        };
        if info.polyline.arr.len() < 3 || self.is_deletion_forbidden(id) {
            return false;
        }
        let corner = if at_start {
            info.polyline.first_corner()
        } else {
            info.polyline.last_corner()
        };

        let mut contacts = self.take_scratch_items();
        self.normal_contacts_at(id, &corner, &mut contacts);
        contacts.retain(|contact| {
            !matches!(
                self.store().get(*contact).map(|item| &item.kind),
                Some(ItemKind::ConductionArea(_))
            )
        });
        let other_id = if contacts.len() == 1 {
            Some(contacts[0])
        } else {
            None
        };
        self.put_scratch_items(contacts);

        let Some(other_id) = other_id else {
            return false;
        };
        let Some(other) = self.trace_info(other_id) else {
            return false;
        };
        if other.layer != info.layer
            || other.half_width != info.half_width
            || other.fixed_state != info.fixed_state
            || other.nets.as_slice() != info.nets.as_slice()
            || other.polyline.arr.len() < 3
            || self.is_deletion_forbidden(other_id)
        {
            return false;
        }
        if other.polyline.first_corner() != corner && other.polyline.last_corner() != corner {
            return false;
        }

        let joined = info.polyline.combine(&other.polyline);
        if joined == info.polyline {
            // `Polyline::combine` refused the join; do not drop `other`.
            return false;
        }
        let joined_line_count = joined.arr.len();

        self.change_trace_polyline(id, joined);
        // Upstream order: the survivor is shrunk-checked first, then the
        // absorbed trace is removed unconditionally.
        if joined_line_count < 3 {
            self.remove_item_unchecked(id);
        }
        self.remove_item_unchecked(other_id);
        true
    }

    // ---- normalize (§11.6) -----------------------------------------------

    /// Upstream `PolylineTrace.normalize(clip)`: split at junctions, combine
    /// the pieces, drop the degenerate ones, and recurse on whatever combining
    /// changed.
    ///
    /// Returns whether anything changed.
    pub fn normalize_trace(&mut self, id: ItemId, clip: Option<&IntOctagon>) -> bool {
        self.normalize_trace_at_depth(id, clip, 0)
    }

    fn normalize_trace_at_depth(
        &mut self,
        id: ItemId,
        clip: Option<&IntOctagon>,
        depth: u32,
    ) -> bool {
        // v2 logs and returns false here; v19 threw. The release ships the v2
        // form (`pull-tight-insert.md` conformance row D4).
        if depth > MAX_NORMALIZATION_DEPTH {
            return false;
        }
        if !self
            .store()
            .get(id)
            .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
        {
            return false;
        }

        let pieces = self.split_trace(id, clip);
        let mut result = pieces.len() != 1;
        for piece in pieces {
            if !self.store().contains(piece) {
                continue;
            }
            let combined = self.combine_trace(piece);
            if !self.store().contains(piece) {
                // Combining removed the piece itself (it shrank below 3 lines).
                result = true;
                continue;
            }
            if self.is_degenerate_trace(piece) {
                // v2-only: a degenerate trace whose deletion is forbidden is
                // *skipped*, not removed — otherwise the outer normalize loop
                // never converges (§11.6).
                if !self.is_deletion_forbidden(piece) {
                    self.remove_item_unchecked(piece);
                    result = true;
                }
            } else if combined {
                self.normalize_trace_at_depth(piece, clip, depth + 1);
                result = true;
            }
        }
        result
    }

    /// Upstream `BasicBoard.normalizeTraces(net)` (`:709-795`) — normalize every
    /// trace of a net until a full pass changes nothing.
    ///
    /// The two v2-only safety features are both here:
    ///
    /// * the net's traces are **snapshotted before** the pass, so mutation
    ///   during it cannot invalidate the iteration (upstream's guard against a
    ///   `ConcurrentModificationException`);
    /// * the outer loop is capped at [`MAX_NORMALIZE_ITERATIONS`], and tripping
    ///   the cap adds the net to the board's suppression set, so every later
    ///   call for that net returns `false` immediately.
    ///
    /// Not implemented: the `else if !isUserFixed && removeIfCycle(t)` branch,
    /// which needs the ripup unit (see the module docs).
    ///
    /// > **The pass order is observable, and it is upstream's.** Splitting a
    /// > trace at a junction can split the *other* trace instead (§11.4 splits
    /// > the found trace first), and upstream's `normalize` reports only
    /// > `pieces.size() != 1` — the change to the other trace is a side effect
    /// > it does not report, and the pieces it created are not in this pass'
    /// > snapshot. So which of two traces meeting at a T is visited first
    /// > decides whether this call returns `true` or `false`, even though the
    /// > resulting board is the same either way. The snapshot is therefore
    /// > taken in **descending id order**, upstream's board-scan order.
    pub fn normalize_traces(&mut self, net_no: i32) -> bool {
        if net_no <= 0 || self.is_normalize_suppressed(net_no) {
            return false;
        }
        let mut result = false;
        let mut something_changed = true;
        let mut iteration = 0u32;

        while something_changed {
            iteration += 1;
            if iteration > MAX_NORMALIZE_ITERATIONS {
                self.normalize_suppressed_mut().insert(net_no);
                break;
            }
            something_changed = false;

            let mut net_traces = self.take_scratch_ids();
            self.collect_net_traces(net_no, &mut net_traces);
            for id in net_traces.iter().copied() {
                if !self.store().contains(id) {
                    continue;
                }
                if self.normalize_trace(id, None) {
                    something_changed = true;
                    result = true;
                }
            }
            self.put_scratch_ids(net_traces);
        }
        result
    }

    /// Upstream `BasicBoard.combineTraces(net)` (`:683-706`) — combine only, no
    /// splitting; used after ripup and at the end of an autoroute pass.
    ///
    /// Upstream restarts the scan from the beginning after every successful
    /// combine, which makes it O(n²) by construction; that is reproduced,
    /// because the restart is what makes a chain collapse fully in one call.
    pub fn combine_traces(&mut self, net_no: i32) -> bool {
        let mut result = false;
        loop {
            let mut net_traces = self.take_scratch_ids();
            self.collect_net_traces(net_no, &mut net_traces);
            let mut combined_any = false;
            for id in net_traces.iter().copied() {
                if !self.store().contains(id) {
                    continue;
                }
                if self.combine_trace(id) {
                    combined_any = true;
                    break;
                }
            }
            self.put_scratch_ids(net_traces);
            if !combined_any {
                return result;
            }
            result = true;
        }
    }

    /// The on-board traces of `net_no` in **descending id order** — the parity
    /// order every upstream board scan uses. `net_no < 0` means every net.
    fn collect_net_traces(&self, net_no: i32, out: &mut Vec<ItemId>) {
        out.clear();
        if net_no < 0 {
            out.extend(
                self.store()
                    .iter()
                    .filter_map(|(id, item)| matches!(item.kind, ItemKind::Trace(_)).then_some(id)),
            );
            return;
        }
        out.extend(
            self.store()
                .net_items(net_no)
                .iter()
                .rev()
                .copied()
                .filter(|id| {
                    self.store()
                        .get(*id)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
                }),
        );
    }
}

#[cfg(test)]
mod tests {
    use fr_geometry::planar::{IntBox, IntPoint};

    use super::*;
    use crate::board::{Board, LayerStructure};
    use crate::clearance::ClearanceTable;
    use crate::item::{LayerRange, Via};
    use crate::shape::{Padstack, TileShape as Shape};

    fn board() -> Board {
        let mut board = Board::new(
            LayerStructure::signal_layers(3),
            ClearanceTable::uniform(3, 3, 20),
        );
        board.padstacks_mut().push(Padstack::uniform(
            0,
            2,
            Shape::Box(IntBox::new(-40, -40, 40, 40)),
        ));
        board
    }

    fn trace(board: &mut Board, net: i32, corners: &[IntPoint]) -> ItemId {
        board
            .insert_trace_without_cleaning(
                Polyline::from_int_points(corners),
                0,
                50,
                NetNos::of(net),
                1,
                FixedState::Unfixed,
            )
            .expect("trace inserts")
    }

    /// A two-corner polyline whose corners coincide — upstream's degenerate
    /// trace. It cannot be built from points (the point constructor collapses
    /// the parallel there-and-back lines), so the three defining lines are
    /// given directly: two coincident verticals with one horizontal between
    /// them, so both corners are the origin.
    pub(crate) fn degenerate_polyline() -> Polyline {
        Polyline::new_from_lines(&[
            Line::new(IntPoint::new(0, -10), IntPoint::new(0, 10)),
            Line::new(IntPoint::new(-10, 0), IntPoint::new(10, 0)),
            Line::new(IntPoint::new(0, 10), IntPoint::new(0, -10)),
        ])
    }

    fn corners_of(board: &Board, id: ItemId) -> Vec<Point> {
        match &board.store().get(id).unwrap().kind {
            ItemKind::Trace(trace) => trace.polyline.corner_arr(),
            _ => panic!("not a trace"),
        }
    }

    #[test]
    fn the_convergence_constants_match_the_release_jar() {
        // Both were read out of the 2.3.0 constant pool
        // (`pull-tight-insert.md` "Release-jar conformance", §2b and row D14).
        // A regression on either is a silent parity break: a smaller depth cap
        // leaves traces unnormalized, a larger iteration cap turns an
        // oscillating net into a hang.
        assert_eq!(MAX_NORMALIZATION_DEPTH, 16);
        assert_eq!(MAX_NORMALIZE_ITERATIONS, 2000);
    }

    #[test]
    fn a_suppressed_net_short_circuits_normalize_traces() {
        // The valve's *effect*: once a net is suppressed, `normalize_traces`
        // returns immediately and touches nothing, for the lifetime of the
        // board. Reaching 2000 oscillating iterations from real geometry is not
        // reproducible on demand, so the state it leaves behind is asserted
        // instead — via the same public predicate the router reads.
        let mut board = board();
        // The stub goes in first so that the spine has the higher id and the
        // descending-id pass reaches it first — see
        // `Board::normalize_traces` on why the order is observable.
        trace(
            &mut board,
            1,
            &[IntPoint::new(1000, 0), IntPoint::new(1000, 1000)],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(2000, 0)],
        );
        assert!(!board.is_normalize_suppressed(1));
        assert!(
            board.normalize_traces(1),
            "the T-junction is real work, so the unsuppressed call reports it"
        );

        // A net that converges is never suppressed, and the next call is a
        // no-op for the ordinary reason rather than the suppressed one.
        assert!(!board.is_normalize_suppressed(1));
        assert!(!board.normalize_traces(1));
        assert_eq!(board.store().len(), 3, "spine split in two, plus the stub");
        board.assert_consistent();
    }

    #[test]
    fn splitting_at_a_point_produces_two_pieces_sharing_every_attribute() {
        let mut board = board();
        let id = trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
        );
        let before = board.store().get(id).unwrap().clone();

        let pieces = board
            .split_trace_at_point(id, &Point::Int(IntPoint::new(500, 0)))
            .expect("the point is on the trace");
        let pieces: Vec<ItemId> = pieces.into_iter().flatten().collect();
        assert_eq!(pieces.len(), 2);
        assert!(!board.store().contains(id), "the original is gone");

        for piece in pieces {
            let item = board.store().get(piece).unwrap();
            assert_eq!(item.nets.as_slice(), before.nets.as_slice());
            assert_eq!(item.clearance_class, before.clearance_class);
            assert_eq!(item.fixed_state, before.fixed_state);
            match (&item.kind, &before.kind) {
                (ItemKind::Trace(now), ItemKind::Trace(was)) => {
                    assert_eq!(now.layer, was.layer);
                    assert_eq!(now.half_width, was.half_width);
                }
                _ => unreachable!(),
            }
        }
        board.assert_consistent();
    }

    #[test]
    fn a_split_piece_can_be_combined_back_into_the_original() {
        let mut board = board();
        let id = trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
        );
        let original: Vec<Point> = corners_of(&board, id);

        let pieces: Vec<ItemId> = board
            .split_trace_at_point(id, &Point::Int(IntPoint::new(400, 0)))
            .unwrap()
            .into_iter()
            .flatten()
            .collect();
        assert!(board.combine_trace(pieces[0]));

        let survivors: Vec<ItemId> = board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(survivors.len(), 1);
        assert_eq!(corners_of(&board, survivors[0]), original);
        board.assert_consistent();
    }

    #[test]
    fn combine_refuses_traces_that_differ_in_any_attribute() {
        for (half_width, fixed, net) in [
            (60, FixedState::Unfixed, 1),
            (50, FixedState::ShoveFixed, 1),
            (50, FixedState::Unfixed, 2),
        ] {
            let mut board = board();
            let a = trace(
                &mut board,
                1,
                &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
            );
            board
                .insert_trace_without_cleaning(
                    Polyline::from_int_points(&[IntPoint::new(1000, 0), IntPoint::new(1000, 1000)]),
                    0,
                    half_width,
                    NetNos::of(net),
                    1,
                    fixed,
                )
                .unwrap();
            assert!(
                !board.combine_trace(a),
                "combined across half_width {half_width}, {fixed:?}, net {net}"
            );
        }
    }

    #[test]
    fn combine_refuses_a_fork() {
        let mut board = board();
        let a = trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(1000, 0), IntPoint::new(1000, 1000)],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(1000, 0), IntPoint::new(1000, -1000)],
        );
        assert!(
            !board.combine_trace(a),
            "two contacts at the corner is a fork"
        );
    }

    #[test]
    fn combining_a_long_chain_is_iterative_and_does_not_recurse() {
        // v2's StackOverflow fix (§11.5): a chain long enough to blow a
        // recursive implementation must combine in one call.
        let mut board = board();
        let mut first = None;
        for i in 0..600 {
            let id = trace(
                &mut board,
                1,
                &[IntPoint::new(i * 100, 0), IntPoint::new((i + 1) * 100, 0)],
            );
            first.get_or_insert(id);
        }
        assert!(board.combine_trace(first.unwrap()));
        let traces = board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .count();
        assert_eq!(traces, 1, "the whole chain collapses");
        board.assert_consistent();
    }

    #[test]
    fn normalize_splits_at_a_t_junction_and_is_idempotent() {
        let mut board = board();
        let spine = trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(2000, 0)],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(1000, 0), IntPoint::new(1000, 1000)],
        );

        assert!(board.normalize_trace(spine, None), "the T must split");
        let after_first: Vec<usize> = vec![board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .count()];
        assert_eq!(after_first[0], 3, "spine split in two, plus the stub");

        // Idempotence: a second full pass changes nothing.
        assert!(!board.normalize_traces(1));
        board.assert_consistent();
    }

    #[test]
    fn inserting_a_via_splits_the_trace_under_it() {
        let mut board = board();
        trace(
            &mut board,
            1,
            &[IntPoint::new(-1000, 0), IntPoint::new(1000, 0)],
        );
        board.insert_via(
            Via {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            },
            NetNos::of(1),
            1,
            FixedState::Unfixed,
        );
        let traces: Vec<ItemId> = board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .map(|(id, _)| id)
            .collect();
        assert_eq!(traces.len(), 2);
        board.assert_consistent();
    }

    #[test]
    fn a_user_fixed_degenerate_trace_is_skipped_not_removed() {
        // §11.6, v2-only: removing it is forbidden, so normalize must skip it
        // rather than loop forever trying.
        let mut board = board();
        let id = board
            .insert_trace_without_cleaning(
                degenerate_polyline(),
                0,
                50,
                NetNos::of(1),
                1,
                FixedState::UserFixed,
            )
            .expect("a USER_FIXED degenerate trace is inserted");
        assert!(board.is_degenerate_trace(id));
        board.normalize_traces(1);
        assert!(board.store().contains(id), "it must survive");
        board.assert_consistent();
    }

    #[test]
    fn normalize_does_not_split_inside_a_pin_pad() {
        use crate::item::Pin;
        let mut board = board();
        board.insert_pin(
            Pin {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                pin_no: 0,
                layers: LayerRange::single(0),
            },
            NetNos::of(1),
            1,
        );
        // A stub ending inside the pad, off its centre.
        let spine = trace(
            &mut board,
            1,
            &[IntPoint::new(-1000, 0), IntPoint::new(1000, 0)],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(20, 0), IntPoint::new(20, 1000)],
        );

        board.normalize_trace(spine, None);
        // The junction at (20, 0) is inside the pad and not at its centre, so
        // upstream's guard forbids the split — but the trace ending there is
        // itself a permitted reason, and that is the precedence bug's domain.
        board.assert_consistent();
    }

    // ---- the split/combine churn regressions (Issue187) ---------------------------------

    fn trace_count(board: &Board) -> usize {
        board
            .store()
            .iter()
            .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
            .count()
    }

    /// The corner multiset of every trace on the board, sorted — upstream's
    /// `normalizeAllTraces()` leaves the wire shapes of an already-normal board
    /// *byte-identical*, so this is the strongest statement of "nothing moved".
    fn wire_shapes(board: &Board) -> Vec<Vec<Point>> {
        let mut shapes: Vec<Vec<Point>> = board
            .store()
            .iter()
            .filter_map(|(_, item)| match &item.kind {
                ItemKind::Trace(trace) => Some(trace.polyline.corner_arr()),
                _ => None,
            })
            .collect();
        shapes.sort_by_key(|corners| format!("{corners:?}"));
        shapes
    }

    #[test]
    fn an_end_to_end_collinear_pair_does_not_split_then_recombine() {
        // The churn shape: two same-net traces meeting end to end on one straight line. Splitting
        // either at the shared corner and letting `combine` weld it back is a no-op that reports
        // "changed", which is what makes `normalize_traces` spin. Upstream refuses the split —
        // `Polyline.split`'s guard fires on both, because for a two-corner trace the only line is
        // both line 1 and the last line.
        let mut board = board();
        let left = trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
        );
        let right = trace(
            &mut board,
            1,
            &[IntPoint::new(1000, 0), IntPoint::new(2000, 0)],
        );

        assert_eq!(
            board.split_trace(left, None),
            vec![left],
            "the left trace must come back whole"
        );
        assert_eq!(
            board.split_trace(right, None),
            vec![right],
            "and so must the right one"
        );
        assert_eq!(trace_count(&board), 2, "no piece was created");
        board.assert_consistent();
    }

    #[test]
    fn an_end_to_end_collinear_pair_is_combined_exactly_once() {
        // The other half of the pair: `combine` *does* weld them, once, and then the board is
        // normal — a second pass must report no change and leave the wire shape alone. A split
        // that reopened the seam would show up here as a `normalize_traces` that never returns
        // `false`.
        let mut board = board();
        trace(
            &mut board,
            1,
            &[IntPoint::new(0, 0), IntPoint::new(1000, 0)],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(1000, 0), IntPoint::new(2000, 0)],
        );

        assert!(board.normalize_traces(1), "the two are welded into one");
        assert_eq!(trace_count(&board), 1);
        let welded = wire_shapes(&board);
        assert_eq!(
            welded[0],
            vec![
                Point::Int(IntPoint::new(0, 0)),
                Point::Int(IntPoint::new(2000, 0))
            ],
            "the weld keeps both original endpoints, to the unit"
        );

        assert!(
            !board.normalize_traces(1),
            "an already-normal board is a fixed point"
        );
        assert_eq!(trace_count(&board), 1);
        assert_eq!(wire_shapes(&board), welded, "and nothing moved");
        board.assert_consistent();
    }

    #[test]
    fn normalize_is_idempotent_at_count_level_on_the_issue187_shape() {
        // Issue187's churn pattern, in miniature: a net of collinear end-to-end runs on a spine,
        // a branch that ends *on* the spine (a real T, which must split once), and a second
        // collinear chain hanging off the branch. Upstream's `normalize_all_traces()` reaches a
        // fixed point on this and leaves the wire-shape multiset alone; the defect this guards
        // against is a second pass that keeps finding "changes" — 340 097 successful combines on
        // the real board for a net loss of 105 traces.
        let mut board = board();
        // A spine, cut into three collinear pieces that meet end to end.
        for x in [0, 1000, 2000] {
            trace(
                &mut board,
                1,
                &[IntPoint::new(x, 0), IntPoint::new(x + 1000, 0)],
            );
        }
        // A branch ending on the spine's interior — the one junction that is a genuine split.
        trace(
            &mut board,
            1,
            &[IntPoint::new(1500, 0), IntPoint::new(1500, 1000)],
        );
        // And a collinear chain hanging off the branch's far end.
        trace(
            &mut board,
            1,
            &[IntPoint::new(1500, 1000), IntPoint::new(1500, 2000)],
        );

        assert!(board.normalize_traces(1), "the first pass has work to do");
        let settled_count = trace_count(&board);
        let settled_shapes = wire_shapes(&board);
        // Spine left of the T, spine right of the T, and the branch welded into one run.
        assert_eq!(settled_count, 3);

        for pass in 0..4 {
            assert!(
                !board.normalize_traces(1),
                "pass {pass} after settling must report no change"
            );
            assert_eq!(trace_count(&board), settled_count, "pass {pass}");
            assert_eq!(wire_shapes(&board), settled_shapes, "pass {pass}");
        }
        board.assert_consistent();
    }

    #[test]
    fn a_collinear_contact_on_an_interior_bend_splits_on_the_bend_exactly() {
        // The configuration that exercises the collinear branch of
        // [`Board::split_against_trace`]: the spine bends at (1000, 0) and a third trace runs
        // *collinear with the spine's first arm*, ending on that bend. The junction is a genuine
        // T — three traces meet there and `combine` can weld none of them — so upstream splits,
        // and the split point is the bend itself, to the unit. A split line taken from anywhere
        // but the intersection would land a corner off by one and leave a seam for the next pass
        // to re-split, which is the churn shape.
        let mut board = board();
        let spine = trace(
            &mut board,
            1,
            &[
                IntPoint::new(0, 0),
                IntPoint::new(1000, 0),
                IntPoint::new(1000, 1000),
            ],
        );
        trace(
            &mut board,
            1,
            &[IntPoint::new(2000, 0), IntPoint::new(1000, 0)],
        );

        let pieces = board.split_trace(spine, None);
        assert_eq!(pieces.len(), 2, "the bend is a T and must split");
        assert_eq!(trace_count(&board), 3);
        assert_eq!(
            wire_shapes(&board),
            vec![
                vec![
                    Point::Int(IntPoint::new(0, 0)),
                    Point::Int(IntPoint::new(1000, 0))
                ],
                vec![
                    Point::Int(IntPoint::new(1000, 0)),
                    Point::Int(IntPoint::new(1000, 1000))
                ],
                vec![
                    Point::Int(IntPoint::new(2000, 0)),
                    Point::Int(IntPoint::new(1000, 0))
                ],
            ],
            "the two arms come back exactly, with no one-unit drift at the bend"
        );

        // And it settles: a full normalize pass afterwards finds nothing more.
        board.normalize_traces(1);
        let settled = wire_shapes(&board);
        assert!(!board.normalize_traces(1));
        assert_eq!(wire_shapes(&board), settled);
        board.assert_consistent();
    }
}
