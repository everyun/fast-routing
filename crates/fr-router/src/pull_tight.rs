//! Post-route trace optimization — upstream's `PullTightAlgo` family.
//!
//! Ported from the 2.3.0 release tree (`/tmp/fr-java-release`, revision
//! `2d4de019`): `board/PullTightAlgo.java` (the framework, 473 L),
//! `board/PullTightAlgo45.java` (the 45° local moves, 622 L) and
//! `RoutingBoard.opt_changed_area`. The spec is
//! `docs/upstream-study/pull-tight-insert.md` §5–§8, whose "Release-jar
//! conformance" appendix **overrides** the spec body.
//!
//! # What is here
//!
//! | item | upstream |
//! |---|---|
//! | [`PullTightAlgo`] | `PullTightAlgo` + `get_instance` (`:71-85`) |
//! | [`PullTightAlgo::opt_changed_area`] | the regional driver (`:88-133`) |
//! | [`PullTightAlgo::check_trace_shape`] | `BasicBoard.check_trace_shape` (`:1166-1221`) |
//! | [`PullTightAlgo::reposition_lines`] | base driver (`:175`) + base `reposition_line` (`:195`) |
//! | [`PullTightAlgo::skip_segments_of_length_0`] | `:292` |
//! | [`PullTightAlgo::trace_pull_tight`] | `PolylineTrace.pull_tight` (`:827-875`) |
//! | `PullTightAlgo::pull_tight_flavor` | `get_instance`'s angle dispatch (`:71-85`) |
//! | [`crate::pull_tight45`] | `PullTightAlgo45` — `reduce_corners`, `smoothen_corners` |
//! | [`crate::pull_tight_any`] | `PullTightAlgoAnyAngle` — the `NONE`-mode rules |
//!
//! # Deliberate deltas from the Java text
//!
//! * **Explicit worklists, not live iterators.** `opt_changed_area` snapshots the
//!   region's trace ids and revalidates each against the store before touching
//!   it, instead of upstream's break-out-of-a-live-`Collection` dance
//!   (`:118-127`). Spec §11 "Iterator invalidation discipline" prescribes this,
//!   and it is what makes the driver safe when a move removes an item.
//! * **The binding-constraint cache** (spec §11, "`check_trace_shape` dominates
//!   runtime"). Every binary search re-queries the tree from scratch upstream;
//!   here the first obstacle that rejected a candidate is remembered as
//!   `(item, tile index)` and re-tested directly before the full query runs. The
//!   candidate shapes of one binary search shrink monotonically toward the same
//!   blocker, so the cached tile answers most rejections without a traversal.
//!   The cache is only ever consulted to prove a *rejection*: an acceptance
//!   always costs the full query, so it cannot change the result.
//! * **`change(new_polyline)` is remove + re-insert** rather than upstream's
//!   in-place `search_tree_manager.change_entries` leaf reuse
//!   (`PolylineTrace.java:942-1009`). That is an allocation optimization, not a
//!   semantic: upstream's own fork-smoothing path re-inserts the same way
//!   (`PullTightAlgo.java:376-381`). Board-visible result is identical; only the
//!   item id changes, which is why the driver's worklist is id-revalidated.
//! * **Fork/end smoothing (`smoothen_end_corners_at_trace`, §6.4) is not
//!   ported.** It is the one part of the family that needs the shove unit
//!   (`insert_trace_without_cleaning` + `split_traces` + `normalize_traces` per
//!   improvement round). The driver's call site is present and returns `false`,
//!   so the net-filtered wrapper semantics of conformance row **D2** are what
//!   lands the day it is filled in.
//! * **90° mode shares the 45° rules**, which is upstream's own dispatch:
//!   `get_instance` returns `PullTightAlgo45` for `NINETY_DEGREE` as well as for
//!   `FORTYFIVE_DEGREE` (`:71-85`), and the rules read the restriction back
//!   where they need it. Any-angle mode has its own flavor, see the table above.

use std::time::{Duration, Instant};

use fr_board::board::Board;
use fr_board::clearance::ClearanceSource;
use fr_board::item::{FixedState, ItemId, ItemKind};
use fr_board::net_nos::NetNos;
use fr_board::search_tree::TreeEntry;
use fr_board::shape::{AngleRestriction, TileShape};
use fr_datastructures::arena_shape_tree::QueryStack;
use fr_datastructures::signum::Signum;
use fr_geometry::planar::{
    IntBox, IntOctagon, Line, Point, Polyline, Side, Simplex,
};
use fr_rules::board_rules::BoardRules;

use crate::insert_forced::ChangedArea;

/// `c_max_cos_angle` (`PullTightAlgo.java:24`) — angles closer than ~2.6° to
/// 180° are numerically unstable and are not divided further.
pub const MAX_COS_ANGLE: f64 = 0.999;

/// `c_min_corner_dist_square` (`PullTightAlgo.java:27`) — corners closer than
/// `sqrt(0.9)` count as equal.
pub const MIN_CORNER_DIST_SQUARE: f64 = 0.9;

/// The floor upstream puts under the user's `trace_pull_tight_accuracy`
/// (`PullTightAlgo.java:83`: `Math.max(p_min_translate_dist, 100)`).
pub const MIN_TRANSLATE_DIST_FLOOR: i32 = 100;

/// The default `pull_tight_accuracy` (`AutorouteControl.java:182`).
pub const DEFAULT_PULL_TIGHT_ACCURACY: i32 = 500;

/// `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` — the per-connection pull-tight budget
/// (`BatchAutorouter.java:57`, used at `:1331`).
///
/// Recorded for reference; the batch driver deliberately does **not** use it,
/// see [`DEFAULT_ROUND_BUDGET`].
pub const TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS: u64 = 1000;

/// `PULL_TIGHT_TIME_LIMIT` — the budget the interactive removal path uses
/// (`RoutingBoard.java:49`).
pub const PULL_TIGHT_TIME_LIMIT_MS: u64 = 2000;

/// The **deterministic** replacement for upstream's wall-clock stall guard: the
/// number of per-polyline optimization rounds one `opt_changed_area` may spend.
///
/// Upstream stops pull-tight after `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` (1000)
/// milliseconds of wall clock. Adopting that verbatim was measurably wrong here:
/// on a loaded machine the same board produced a different `.ses` (56 vs 34
/// incomplete connections on `Issue034`) purely because fewer rounds fitted in
/// the second, and this crate's documented contract is that every ordering and
/// stopping input is an integer (see the crate docs, "Determinism", and
/// `tests/determinism.rs`).
///
/// A round budget is the same *kind* of guard — both exist only to stop a
/// pathological region from spinning forever, since every individual move is
/// already monotone (spec §5.5) — but it is reproducible. The value is far above
/// what any fixture reaches, so it is a safety valve and not a quality knob; a
/// board that trips it gets upstream's behaviour of leaving the region partly
/// optimized.
pub const DEFAULT_ROUND_BUDGET: u64 = 2_000_000;

/// The numeric backoff every binary search takes when a translation overshot
/// its own side test (`shorten_value = sign * 0.5`, all loops).
pub const NUMERIC_BACKOFF: f64 = 0.5;

/// The obstacle that rejected the previous candidate shape, remembered so the
/// next candidate can be rejected without a tree traversal.
#[derive(Debug, Clone, Copy)]
struct BindingObstacle {
    item: ItemId,
    /// Index of the blocking tile inside the item's tree shapes.
    shape_index: u16,
    /// Store revision the cache was taken at; anything else invalidates it.
    revision: u64,
}

/// Everything `prepare_trace` reads off a trace before optimizing it, so the
/// board borrow can be released before the optimization mutates it.
struct TracePrep {
    layer: i32,
    half_width: i32,
    polyline: Polyline,
    clearance_class: i32,
    fixed_state: FixedState,
    nets: NetNos,
}

/// `PullTightAlgo` — the trace optimizer, bound to one board for its lifetime.
///
/// Construct with [`PullTightAlgo::new`], which is upstream's `get_instance`
/// (`:71-85`) minus the angle dispatch: the 45° rules are the only ones ported,
/// see the module docs.
pub struct PullTightAlgo<'b> {
    /// The board being optimized.
    pub board: &'b mut Board,
    rules: &'b BoardRules,
    /// When non-empty, only traces whose net array equals this are optimized
    /// (`PolylineTrace.java:838`, and the D2 wrapper filter).
    only_net_no_arr: Vec<i32>,
    /// Moves whose touched corners fall outside this are rejected (`:96`).
    curr_clip_shape: Option<IntOctagon>,
    /// Termination granularity of every binary search, in board units.
    min_translate_dist: f64,
    keep_point: Option<Point>,
    keep_point_layer: i32,
    /// Optional wall-clock cutoff. `None` — the batch default — keeps the run
    /// reproducible; see [`DEFAULT_ROUND_BUDGET`].
    deadline: Option<Instant>,
    /// The deterministic stall guard, decremented once per optimization round.
    round_budget: u64,
    /// Set when a guard fired, so callers can tell "converged" from "gave up".
    budget_exhausted: bool,

    // ---- per-trace working state (`pull_tight(Polyline,…)`, `:140-148`) ----
    curr_layer: i32,
    curr_half_width: i32,
    curr_net_no_arr: NetNos,
    curr_cl_type: i32,
    /// The pins at the trace's end corners. `None` is upstream's `null`, which
    /// switches the acid-trap pin rule off entirely (`:1183-1190`).
    contact_pins: Option<Vec<ItemId>>,

    bounds: IntBox,
    binding: Option<BindingObstacle>,

    // ---- scratch, reused across every candidate (spec §11 "Arena the polylines") ----
    scratch_entries: Vec<TreeEntry>,
    scratch_stack: QueryStack,
    scratch_ids: Vec<ItemId>,
    scratch_lines: Vec<Line>,
    check_lines: [Line; 3],
}

impl<'b> PullTightAlgo<'b> {
    /// `PullTightAlgo.get_instance` (`:71-85`).
    ///
    /// `min_translate_dist` is floored at [`MIN_TRANSLATE_DIST_FLOOR`] exactly as
    /// upstream does (`:83`). `time_limit` of `None` means "no limit"; upstream
    /// spells that `p_time_limit <= 0` (`:56-60`).
    // Upstream's `get_instance` takes the same eight; keeping them positional
    // keeps the call sites diffable against the Java.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        board: &'b mut Board,
        rules: &'b BoardRules,
        only_net_no_arr: &[i32],
        clip_shape: Option<IntOctagon>,
        min_translate_dist: i32,
        time_limit: Option<Duration>,
        keep_point: Option<Point>,
        keep_point_layer: i32,
    ) -> PullTightAlgo<'b> {
        let bounds = board_bounds(board);
        let mut nets = only_net_no_arr.to_vec();
        nets.sort_unstable();
        nets.dedup();
        let placeholder = Line::new_from_coords(0, 0, 1, 0);
        PullTightAlgo {
            board,
            rules,
            only_net_no_arr: nets,
            curr_clip_shape: clip_shape,
            min_translate_dist: f64::from(min_translate_dist.max(MIN_TRANSLATE_DIST_FLOOR)),
            keep_point,
            keep_point_layer,
            deadline: time_limit.map(|limit| Instant::now() + limit),
            round_budget: DEFAULT_ROUND_BUDGET,
            budget_exhausted: false,
            curr_layer: 0,
            curr_half_width: 0,
            curr_net_no_arr: NetNos::default(),
            curr_cl_type: 0,
            contact_pins: None,
            bounds,
            binding: None,
            scratch_entries: Vec::new(),
            scratch_stack: QueryStack::new(),
            scratch_ids: Vec::new(),
            scratch_lines: Vec::new(),
            check_lines: [placeholder; 3],
        }
    }

    /// `is_stop_requested` (`:150-170`), with the round budget standing in for
    /// upstream's wall clock. This crate has no stoppable thread.
    ///
    /// The budget is checked first and is the only guard the batch driver arms,
    /// so a run's output does not depend on how fast the machine is.
    pub fn is_stop_requested(&self) -> bool {
        if self.round_budget == 0 {
            return true;
        }
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    /// Consumes one round of the deterministic budget.
    ///
    /// Returns false once the budget is gone, which is what ends every
    /// optimization loop.
    pub(crate) fn take_round(&mut self) -> bool {
        match self.round_budget.checked_sub(1) {
            Some(remaining) => {
                self.round_budget = remaining;
                true
            }
            None => {
                self.budget_exhausted = true;
                false
            }
        }
    }

    /// True when a stall guard stopped the optimization before it converged.
    #[must_use]
    pub fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    // ---- §5.2 the regional driver ----------------------------------------

    /// `opt_changed_area` (`:88-133`) — optimize every trace overlapping the
    /// per-layer changed region, until a whole round changes nothing.
    ///
    /// `changed_area` is consumed as upstream consumes `board.changed_area`:
    /// each layer's region is read, **cleared**, then enlarged by
    /// `1.5 * (max_clearance(layer) + 2 * max_trace_half_width)` (`:108-109`).
    /// Every accepted move re-joins its touched corners into it, so the loop
    /// chases ripples to a fixed point — or to the time limit.
    ///
    /// Returns true when anything changed.
    ///
    /// The via branch (`OptViaAlgo.opt_via_location`, `:128`) is not ported;
    /// vias are left where the inserter put them.
    pub fn opt_changed_area(&mut self, changed_area: &mut ChangedArea) -> bool {
        let mut anything_changed = false;
        let mut something_changed = true;
        while something_changed {
            something_changed = false;
            for layer in 0..changed_area.layer_count() as i32 {
                let region = changed_area.area(layer);
                if region.is_empty() {
                    continue;
                }
                changed_area.set_empty(layer);
                let offset = 1.5
                    * f64::from(
                        self.max_clearance_on_layer(layer) + 2 * self.board.max_trace_half_width(),
                    );
                let region = region.offset(offset);

                // The explicit worklist: snapshot the ids, then revalidate each
                // one before use. Upstream iterates a live collection and breaks
                // out of it whenever an item may have been removed (`:121-127`).
                let mut candidates = Vec::new();
                self.board.overlapping_items(
                    &TileShape::Simplex(region.to_simplex()),
                    Some(layer),
                    &[],
                    &mut candidates,
                );
                candidates.retain(|id| {
                    self.board
                        .store()
                        .get(*id)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
                });
                candidates.sort_unstable();

                for id in candidates {
                    if self.is_stop_requested() {
                        return anything_changed;
                    }
                    // Liveness: a previous move may have removed, split or
                    // replaced this trace.
                    if !self
                        .board
                        .store()
                        .get(id)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
                    {
                        continue;
                    }
                    if self.trace_pull_tight(id, changed_area).is_some() {
                        something_changed = true;
                        anything_changed = true;
                        if self.split_traces_at_keep_point() {
                            break;
                        }
                    }
                    // `else if smoothen_end_corners_at_trace(t)` — §6.4, the
                    // net-filtered wrapper of conformance row D2. Not ported;
                    // see the module docs.
                }
            }
        }
        anything_changed
    }

    /// `rules.clearance_matrix.max_value(layer)` (`:108`).
    fn max_clearance_on_layer(&self, layer: i32) -> i32 {
        let clearances = self.board.clearances();
        let class_count = clearances.class_count() as i32;
        let mut max = 0;
        for i in 0..class_count {
            for j in i..class_count {
                max = max.max(clearances.value(i, j, layer));
            }
        }
        max
    }

    // ---- §5.4 the per-trace entry point ----------------------------------

    /// `PolylineTrace.pull_tight` (`:827-875`) — the guards, the optimize call,
    /// and `change(new_lines)` on success.
    ///
    /// Returns the trace's id after the change (a new id, see the module docs),
    /// or `None` when nothing was changed. The trace may be **gone** on success
    /// — normalization can delete a piece that collapsed — in which case the
    /// returned id is not in the store.
    ///
    /// The `swap_connection_to_pin` / `correct_connection_to_pin` ladder
    /// (`:855-873`) is gated on `rules.pin_edge_to_turn_dist() > 0` upstream and
    /// is not ported; the gate itself is honoured by
    /// [`crate::insert`], which forces the knob to `-1` for the duration of an
    /// insertion so this path stays off exactly when upstream turns it off.
    pub fn trace_pull_tight(&mut self, id: ItemId, changed_area: &mut ChangedArea) -> Option<ItemId> {
        let prepared = self.prepare_trace(id)?;
        let TracePrep {
            layer,
            half_width,
            polyline,
            clearance_class,
            fixed_state,
            nets,
        } = prepared;

        let new_polyline = self.pull_tight_flavor(&polyline, changed_area)?;

        // `change(new_polyline)` (`PolylineTrace.java:942-1009`): swap the
        // geometry, then normalize against the changed-area clip.
        self.board.remove_item(id);
        let new_id = self.board.insert_trace_without_cleaning(
            new_polyline,
            layer,
            half_width,
            nets,
            clearance_class,
            fixed_state,
        )?;
        // `change` ends in `this.normalize(clip_shape)` and nothing else
        // (`PolylineTrace.java:1004`); `normalize` itself splits and then
        // combines each piece, so an extra `combine_trace` here would be a
        // second, unsynchronised merge pass.
        let clip = changed_area.area(layer);
        self.board.normalize_trace(new_id, Some(&clip));
        Some(new_id)
    }

    /// `PullTightAlgo.get_instance`'s flavor dispatch (`:71-85`).
    ///
    /// | board angle restriction | upstream class | here |
    /// |---|---|---|
    /// | `NINETY_DEGREE` | `PullTightAlgo45` | [`crate::pull_tight45`] |
    /// | `FORTYFIVE_DEGREE` | `PullTightAlgo45` | [`crate::pull_tight45`] |
    /// | `NONE` | `PullTightAlgoAnyAngle` | [`crate::pull_tight_any`] |
    ///
    /// Upstream chooses the subclass once, in the constructor, from
    /// `p_board.rules.get_trace_angle_restriction()`; the same value is read
    /// here per trace because the flavor is a pure function of it and this port
    /// has one struct rather than three.
    pub(crate) fn pull_tight_flavor(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        match self.board.angle_restriction() {
            AngleRestriction::None => {
                self.pull_tight_polyline_any_angle(polyline, changed_area)
            }
            _ => self.pull_tight_polyline(polyline, changed_area),
        }
    }

    /// Runs `PolylineTrace.pull_tight`'s guards (`:832-851`) and installs the
    /// per-trace working state of `pull_tight(Polyline, …)` (`:140-148`).
    ///
    /// `None` means a guard refused the trace, which is upstream's "return
    /// false without touching anything".
    fn prepare_trace(&mut self, id: ItemId) -> Option<TracePrep> {
        let item = self.board.store().get(id)?;
        let ItemKind::Trace(trace) = &item.kind else {
            return None;
        };
        // `:832-843` — on board, not shove fixed, normal nets, net filter.
        if item.is_shove_fixed() || !item.nets_normal() {
            return None;
        }
        let nets = item.nets.clone();
        if !self.only_net_no_arr.is_empty() && nets.as_slice() != self.only_net_no_arr.as_slice() {
            return None;
        }
        // `:844-851` — the per-net-class `get_pull_tight()` disable knob.
        if let Some(&net_no) = nets.as_slice().first() {
            if !self.net_class_pull_tight(net_no) {
                return None;
            }
        }

        let prep = TracePrep {
            layer: trace.layer,
            half_width: trace.half_width,
            polyline: trace.polyline.clone(),
            clearance_class: item.clearance_class,
            fixed_state: item.fixed_state,
            nets,
        };

        // The oracle must query a tree that has this trace's clearance baked
        // into its obstacles. The driver optimizes **every** net in the region
        // (upstream passes `new int[0]`, `BatchAutorouter.java:1330`), so the
        // class here is routinely one the maze engine never built a tree for —
        // and falling back to the *uncompensated* default tree would silently
        // check with zero clearance and accept violating moves.
        //
        // Upstream's escape from the same corner is
        // `overlapping_tree_entries_with_clearance` (`BasicBoard.java:1176`);
        // building the class' tree once is the same answer, and it is cached
        // for every later trace of the class.
        self.board.ensure_compensated_tree(prep.clearance_class);

        // `:140-148` — the working state, with the tree's clearance
        // compensation folded into the half width.
        self.curr_layer = prep.layer;
        self.curr_half_width = prep.half_width
            + self
                .board
                .clearances()
                .compensation_value(prep.clearance_class, prep.layer);
        self.curr_net_no_arr = prep.nets.clone();
        self.curr_cl_type = prep.clearance_class;
        self.contact_pins = Some(self.touching_pins_at_end_corners(
            &prep.polyline,
            prep.layer,
            prep.half_width,
            &prep.nets,
            prep.clearance_class,
        ));
        self.binding = None;
        Some(prep)
    }

    /// How many of the trace's own segments currently fail the oracle.
    ///
    /// `None` when `id` is not a trace this optimizer would touch. Installs the
    /// same working state [`PullTightAlgo::trace_pull_tight`] would, so the
    /// answer is the one pull-tight itself would compute.
    pub fn trace_polyline_violations(&mut self, id: ItemId) -> Option<usize> {
        let prep = self.prepare_trace(id)?;
        Some(self.polyline_violation_count(&prep.polyline))
    }

    /// The net class' `get_pull_tight()` flag (`PolylineTrace.java:845-850`).
    fn net_class_pull_tight(&self, net_no: i32) -> bool {
        self.rules
            .nets
            .get(net_no)
            .map(|net| net.net_class_index())
            .and_then(|index| self.rules.net_classes.get(index))
            .is_none_or(|class| class.pull_tight)
    }

    /// `Trace.touching_pins_at_end_corners` (`Trace.java:386-404`): pins of the
    /// own net overlapping the octagon around either end corner, enlarged by the
    /// half width.
    ///
    /// These are the pins pull-tight is allowed to touch. **Every other pin is
    /// an obstacle, even one of the own net** — that is the acid-trap rule, and
    /// it is the whole reason this set has to be computed
    /// (`BasicBoard.java:1183-1190`).
    fn touching_pins_at_end_corners(
        &mut self,
        polyline: &Polyline,
        layer: i32,
        half_width: i32,
        nets: &NetNos,
        clearance_class: i32,
    ) -> Vec<ItemId> {
        let mut result = Vec::new();
        for corner in [polyline.first_corner(), polyline.last_corner()] {
            let octagon = corner.surrounding_octagon().offset(f64::from(half_width));
            let query = TileShape::Simplex(octagon.to_simplex());
            let PullTightAlgo {
                board,
                scratch_ids,
                scratch_stack,
                ..
            } = self;
            let tree = board
                .trees()
                .compensated_tree(clearance_class)
                .unwrap_or_else(|| board.default_tree());
            fr_metrics::counters::bump::tree_queries(1);
            tree.overlapping_items(
                board.store(),
                &query,
                Some(layer),
                &[],
                scratch_stack,
                scratch_ids,
            );
            for &id in scratch_ids.iter() {
                if let Some(item) = board.store().get(id) {
                    if matches!(item.kind, ItemKind::Pin(_)) && item.nets.shares_any(nets) {
                        result.push(id);
                    }
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    /// `split_traces_at_keep_point` (`:411-427`): the keep point must stay a
    /// corner of the net, so every trace running through it is split there.
    ///
    /// Returns true when something was split — upstream's signal to restart the
    /// driver's region scan (`:124`).
    pub fn split_traces_at_keep_point(&mut self) -> bool {
        let Some(keep_point) = self.keep_point.clone() else {
            return false;
        };
        let layer = self.keep_point_layer;
        let mut picked = Vec::new();
        self.board
            .pick_items(&keep_point, Some(layer), &mut picked);
        picked.retain(|id| {
            self.board
                .store()
                .get(*id)
                .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
        });
        picked.sort_unstable();
        for id in picked {
            if let Some(pieces) = self.board.split_trace_at_point(id, &keep_point) {
                if pieces.iter().filter(|piece| piece.is_some()).count() > 1 {
                    return true;
                }
            }
        }
        false
    }

    /// The trace `pick_items(point, layer, TRACES).iterator().next()` answers
    /// (`RoutingBoard.java:857-871`).
    ///
    /// `pick_items` builds a `TreeSet<Item>` and `Item.compareTo` is
    /// `other.id_no - this.id_no` (`Item.java:94-99`), so the set iterates
    /// **descending by id** and upstream's `next()` is the *highest* id at the
    /// point. That is the trace `insert_forced_trace_polyline` re-reads its
    /// `new_trace` handle from after a split invalidated the old one, so the
    /// tie-break is routing-visible and is reproduced exactly.
    pub(crate) fn picked_trace_at(&mut self, point: &Point, layer: i32) -> Option<ItemId> {
        let mut picked = Vec::new();
        self.board.pick_items(point, Some(layer), &mut picked);
        picked
            .into_iter()
            .filter(|id| {
                self.board
                    .store()
                    .get(*id)
                    .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
            })
            .max()
    }

    // ---- §5.3 the clearance oracle ---------------------------------------

    /// `BasicBoard.check_trace_shape(shape, layer, nets, cl, contact_pins)`
    /// (`:1166-1221`).
    ///
    /// The three obstacle rules, in upstream's order:
    ///
    /// 1. an item in `contact_pins` is skipped;
    /// 2. with `contact_pins` set, **any other pin is an obstacle** even on the
    ///    own net — acid-trap prevention (`:1186-1190`);
    /// 3. otherwise an item is an obstacle unless `is_trace_obstacle` is false
    ///    for one of the routed nets.
    ///
    /// The tie-pin tolerance of `:1198-1217` (a foreign-net trace overlapping
    /// only inside a shared tie pin's pad is not an obstacle) is not ported:
    /// it needs multi-net pins, which no fixture in the gate has. Its absence is
    /// conservative — it can only make a move be rejected, never accepted.
    pub fn check_trace_shape(&mut self, shape: &Simplex) -> bool {
        if !shape_is_contained_in(shape, &self.bounds) {
            return false;
        }
        // The binding-constraint fast path: re-test the tile that rejected the
        // previous candidate. Only ever used to prove a rejection.
        if let Some(binding) = self.binding {
            if self.binding_still_blocks(&binding, shape) {
                return false;
            }
        }

        let layer = self.curr_layer;
        let tree_class = self.curr_cl_type;
        let query = TileShape::Simplex(shape.clone());
        fr_metrics::counters::bump::tree_queries(1);
        let PullTightAlgo {
            board,
            scratch_entries,
            scratch_stack,
            contact_pins,
            curr_net_no_arr,
            binding,
            ..
        } = self;
        let tree = board
            .trees()
            .compensated_tree(tree_class)
            .unwrap_or_else(|| board.default_tree());
        tree.overlapping_tree_entries(
            board.store(),
            &query,
            Some(layer),
            &[],
            scratch_stack,
            scratch_entries,
        );
        for entry in scratch_entries.iter() {
            let Some(item) = board.store().get(entry.item) else {
                continue;
            };
            if let Some(pins) = contact_pins.as_ref() {
                if pins.binary_search(&entry.item).is_ok() {
                    continue;
                }
                if matches!(item.kind, ItemKind::Pin(_)) {
                    *binding = board.store().revision_of(entry.item).map(|revision| {
                        BindingObstacle {
                            item: entry.item,
                            shape_index: entry.shape_index,
                            revision,
                        }
                    });
                    return false;
                }
            }
            let mut is_obstacle = true;
            for &net_no in curr_net_no_arr.as_slice() {
                if !item.is_trace_obstacle(net_no) {
                    is_obstacle = false;
                }
            }
            if is_obstacle {
                *binding = board
                    .store()
                    .revision_of(entry.item)
                    .map(|revision| BindingObstacle {
                        item: entry.item,
                        shape_index: entry.shape_index,
                        revision,
                    });
                return false;
            }
        }
        true
    }

    /// True when the cached blocker still exists unchanged **and** still
    /// overlaps `shape`, i.e. the candidate is rejected without a traversal.
    fn binding_still_blocks(&self, binding: &BindingObstacle, shape: &Simplex) -> bool {
        if self.board.store().revision_of(binding.item) != Some(binding.revision) {
            return false;
        }
        let Some(slot) = self.board.store().slot_of(binding.item) else {
            return false;
        };
        let tree_class = self.curr_cl_type;
        let tree = self
            .board
            .trees()
            .compensated_tree(tree_class)
            .unwrap_or_else(|| self.board.default_tree());
        if tree.shape_layer(slot, binding.item, binding.shape_index) != Some(self.curr_layer) {
            return false;
        }
        let Some(tile) = tree.tile_shape(slot, binding.item, binding.shape_index) else {
            return false;
        };
        TileShape::Simplex(shape.clone()).intersects(tile)
    }

    // ---- §6.1 base moves shared by every angle mode ----------------------

    /// `reposition_lines` (`:175-190`) — try to shorten the polyline by
    /// translating one interior line, first success wins.
    pub fn reposition_lines(
        &mut self,
        polyline: &Polyline,
        changed_area: &mut ChangedArea,
    ) -> Option<Polyline> {
        if polyline.arr.len() < 5 {
            return None;
        }
        for index in 2..polyline.arr.len() - 2 {
            let Some(new_line) = self.reposition_line(&polyline.arr, index, changed_area) else {
                continue;
            };
            self.scratch_lines.clear();
            self.scratch_lines.extend_from_slice(&polyline.arr);
            self.scratch_lines[index] = new_line;
            let result = Polyline::new_from_lines(&self.scratch_lines);
            if result.arr.is_empty() {
                continue;
            }
            return Some(
                self.skip_segments_of_length_0(&result)
                    .unwrap_or(result),
            );
        }
        None
    }

    /// Base `reposition_line` (`:195-290`) — binary-search the largest
    /// translation of line `no` toward the nearer of its two outer corners that
    /// still passes [`PullTightAlgo::check_trace_shape`].
    ///
    /// The three termination guards are upstream's, verbatim, and all three are
    /// load-bearing (spec §5.5):
    ///
    /// * the loop ends when `|delta| <= min_translate_dist`;
    /// * a first-try translation that lands on the *wrong side* of the nearest
    ///   point is backed off by [`NUMERIC_BACKOFF`] and retried, rather than
    ///   accepted (`:245-255`);
    /// * a translation that does not move the line at all aborts (`:238-242`).
    fn reposition_line(
        &mut self,
        line_arr: &[Line],
        no: usize,
        changed_area: &mut ChangedArea,
    ) -> Option<Line> {
        if line_arr.len() - no < 3 || no < 2 {
            return None;
        }
        // `:203-213` — both corners of the line to translate must be inside the
        // clip shape.
        if let Some(clip) = self.curr_clip_shape {
            for offset in 0..2 {
                let corner = line_arr[no - 1 + offset].intersection(&line_arr[no + offset]);
                if octagon_is_outside(&clip, &corner) {
                    return None;
                }
            }
        }

        let translate_line = line_arr[no];
        let prev_corner = line_arr[no - 2].intersection(&line_arr[no - 1]);
        let next_corner = line_arr[no + 1].intersection(&line_arr[no + 2]);
        let prev_dist = translate_line.signed_distance(&prev_corner.to_float());
        let next_dist = translate_line.signed_distance(&next_corner.to_float());
        if Signum::of(prev_dist) != Signum::of(next_dist) {
            // `:221-224` — the corners straddle the line; translating cannot
            // shorten it.
            return None;
        }
        let (nearest_point, max_translate_dist) = if prev_dist.abs() < next_dist.abs() {
            (prev_corner, prev_dist)
        } else {
            (next_corner, next_dist)
        };
        let mut translate_dist = max_translate_dist;
        let mut delta_dist = max_translate_dist;
        let side_of_nearest_point = translate_line.side_of_point(&nearest_point);
        let sign = signum_as_int(max_translate_dist);
        let mut new_line: Option<Line> = None;
        self.check_lines[0] = line_arr[no - 1];
        self.check_lines[2] = line_arr[no + 1];
        let mut first_time = true;

        while first_time || delta_dist.abs() > self.min_translate_dist {
            // `:230-237` — the first candidate is the exact line through the
            // nearest corner when that corner is integral; every later one is a
            // float translation.
            self.check_lines[1] = match (first_time, &nearest_point) {
                (true, Point::Int(point)) => {
                    Line::new_from_point_and_dir(*point, &translate_line.direction())
                }
                _ => translate_line.translate(-translate_dist),
            };
            if self.check_lines[1] == translate_line {
                return None;
            }
            let new_side = self.check_lines[1].side_of_point(&nearest_point);
            if new_side != side_of_nearest_point && new_side != Side::Collinear {
                // `:245-255` — overshot because `nearest_point` was not
                // integral; back off and retry.
                //
                // Upstream also decrements `max_translate_dist` here, but the
                // base version never reads it again — unlike
                // `PullTightAlgo45.smoothen_corner` (`:398-403`), where the very
                // same decrement *is* load-bearing because the accept branch
                // compares `translate_dist == max_translate_dist`. So it is
                // dropped here and kept there.
                let shorten = f64::from(sign) * NUMERIC_BACKOFF;
                translate_dist -= shorten;
                delta_dist -= shorten;
                continue;
            }
            let mut check_ok = false;
            let tmp = Polyline::new_from_lines(&self.check_lines);
            if tmp.arr.len() == 3 {
                if let Some(shape) = tmp.offset_shape(self.curr_half_width, 0) {
                    check_ok = self.check_trace_shape(&shape);
                }
            }
            delta_dist /= 2.0;
            if check_ok {
                new_line = Some(self.check_lines[1]);
                if first_time {
                    // `:272-275` — biggest possible change, stop early.
                    break;
                }
                translate_dist += delta_dist;
            } else {
                translate_dist -= delta_dist;
            }
            first_time = false;
        }

        if let Some(line) = new_line {
            // `:283-289` — mark both the new and the old corners.
            let layer = self.curr_layer;
            changed_area.join_point(self.check_lines[0].intersection_approx(&line), layer);
            changed_area.join_point(self.check_lines[2].intersection_approx(&line), layer);
            changed_area
                .join_point(line_arr[no - 1].intersection_approx(&line_arr[no]), layer);
            changed_area
                .join_point(line_arr[no].intersection_approx(&line_arr[no + 1]), layer);
        }
        new_line
    }

    /// `skip_segments_of_length_0` (`:292-341`) — drop lines whose two corners
    /// coincide, re-checking clearance because removing one can create a dog ear.
    ///
    /// The first and last corner positions are retained **exactly** (`:298-303`
    /// uses exact `Point` equality there and the
    /// [`MIN_CORNER_DIST_SQUARE`] float test everywhere else), and 45° lines
    /// skip the clearance check entirely because their shapes are intersected
    /// with the bounding octagon anyway (`:318-321`).
    ///
    /// Takes no changed area: upstream does not mark one here either, because
    /// removing a zero-length segment moves no copper — the corners it joins
    /// were already coincident.
    pub fn skip_segments_of_length_0(&mut self, polyline: &Polyline) -> Option<Polyline> {
        let mut polyline_changed = false;
        let mut curr = polyline.clone();
        let mut i = 1usize;
        while i + 1 < curr.arr.len() {
            let try_skip = if i == 1 || i == curr.arr.len() - 2 {
                curr.corner(i) == curr.corner(i - 1)
            } else {
                curr.corner_approx(i)
                    .distance_square(&curr.corner_approx(i - 1))
                    < MIN_CORNER_DIST_SQUARE
            };
            if try_skip {
                self.scratch_lines.clear();
                self.scratch_lines.extend_from_slice(&curr.arr[..i]);
                self.scratch_lines.extend_from_slice(&curr.arr[i + 1..]);
                let expected = self.scratch_lines.len();
                let tmp = Polyline::new_from_lines(&self.scratch_lines);
                let mut check_ok = tmp.arr.len() == expected;
                if check_ok && !curr.arr[i].is_multiple_of_45_degree() {
                    if i > 1 {
                        check_ok = match tmp.offset_shape(self.curr_half_width, i - 2) {
                            Some(shape) => self.check_trace_shape(&shape),
                            None => false,
                        };
                    }
                    if check_ok && i + 2 < curr.arr.len() {
                        check_ok = match tmp.offset_shape(self.curr_half_width, i - 1) {
                            Some(shape) => self.check_trace_shape(&shape),
                            None => false,
                        };
                    }
                }
                if check_ok {
                    polyline_changed = true;
                    curr = tmp;
                    i = i.saturating_sub(1).max(1);
                    continue;
                }
            }
            i += 1;
        }
        if polyline_changed {
            Some(curr)
        } else {
            None
        }
    }

    /// How many of `polyline`'s segments fail the clearance oracle, using the
    /// working state of the trace currently being optimized.
    ///
    /// This is the invariant pull-tight is supposed to preserve: it may leave a
    /// segment that was *already* violating alone, but it must never turn a
    /// clean segment dirty. Tests assert on it, and it is how the phase that
    /// introduces a violation gets identified.
    ///
    /// Only meaningful between [`PullTightAlgo::trace_pull_tight`]'s working
    /// state being set and the next trace being started.
    pub fn polyline_violation_count(&mut self, polyline: &Polyline) -> usize {
        let mut bad = 0;
        for index in 0..polyline.arr.len().saturating_sub(2) {
            if let Some(shape) = polyline.offset_shape(self.curr_half_width, index) {
                // The binding cache is a rejection shortcut keyed on the last
                // candidate; clear it so each segment gets a full, independent
                // answer.
                self.binding = None;
                if !self.check_trace_shape(&shape) {
                    bad += 1;
                }
            }
        }
        bad
    }

    // ---- accessors the 45° rules need ------------------------------------

    /// The compensated half width the current trace is checked at.
    #[inline]
    pub(crate) fn curr_half_width(&self) -> i32 {
        self.curr_half_width
    }

    /// The layer the current trace is on.
    #[inline]
    pub(crate) fn curr_layer(&self) -> i32 {
        self.curr_layer
    }

    /// The clip shape moves are restricted to, if any.
    #[inline]
    pub(crate) fn clip_shape(&self) -> Option<IntOctagon> {
        self.curr_clip_shape
    }

    /// The binary-search termination granularity.
    #[inline]
    pub(crate) fn min_translate_dist(&self) -> f64 {
        self.min_translate_dist
    }
}

/// `IntOctagon.is_outside(Point)` — outside any of the eight half planes.
///
/// Lives here because `fr_geometry`'s `IntOctagon` does not expose it and that
/// crate is gated.
pub(crate) fn octagon_is_outside(octagon: &IntOctagon, point: &Point) -> bool {
    let Point::Int(p) = point else {
        // A rational corner: fall back to its float position, which is what
        // upstream's `is_outside(FloatPoint)` overload does.
        let f = point.to_float();
        return f.x < f64::from(octagon.left_x)
            || f.y < f64::from(octagon.bottom_y)
            || f.x > f64::from(octagon.right_x)
            || f.y > f64::from(octagon.top_y)
            || (f.x - f.y) < f64::from(octagon.upper_left_diagonal_x)
            || (f.x - f.y) > f64::from(octagon.lower_right_diagonal_x)
            || (f.x + f.y) < f64::from(octagon.lower_left_diagonal_x)
            || (f.x + f.y) > f64::from(octagon.upper_right_diagonal_x);
    };
    let (x, y) = (i64::from(p.x), i64::from(p.y));
    x < i64::from(octagon.left_x)
        || y < i64::from(octagon.bottom_y)
        || x > i64::from(octagon.right_x)
        || y > i64::from(octagon.top_y)
        || (x - y) < i64::from(octagon.upper_left_diagonal_x)
        || (x - y) > i64::from(octagon.lower_right_diagonal_x)
        || (x + y) < i64::from(octagon.lower_left_diagonal_x)
        || (x + y) > i64::from(octagon.upper_right_diagonal_x)
}

/// `Signum.as_int` — the sign of a double as -1 / 0 / 1.
pub(crate) fn signum_as_int(value: f64) -> i32 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

/// `TileShape.is_contained_in(bounding_box)` for a simplex.
fn shape_is_contained_in(shape: &Simplex, bounds: &IntBox) -> bool {
    for index in 0..shape.border_line_count() {
        if !shape.corner_is_bounded(index) {
            return false;
        }
        let corner = shape.corner_approx(index);
        if corner.x < f64::from(bounds.ll.x)
            || corner.y < f64::from(bounds.ll.y)
            || corner.x > f64::from(bounds.ur.x)
            || corner.y > f64::from(bounds.ur.y)
        {
            return false;
        }
    }
    true
}

/// The margin `RoutingBoard` pads its outline with, matching
/// [`crate::engine`]'s `BOUNDS_MARGIN` so both units agree on what "on the
/// board" means.
const BOUNDS_MARGIN: i32 = 1000;

/// The board bounding box: the union of every indexed tile shape plus
/// [`BOUNDS_MARGIN`], upstream's `RoutingBoard.bounding_box`.
///
/// Derived exactly the way [`crate::engine`] derives it, so a shape the maze
/// engine considered on-board is never rejected here as off-board.
fn board_bounds(board: &Board) -> IntBox {
    let mut bounds: Option<IntBox> = None;
    for (id, item) in board.store().iter() {
        if matches!(item.kind, ItemKind::ComponentOutline(_)) {
            continue;
        }
        let Some(slot) = board.store().slot_of(id) else {
            continue;
        };
        for tile in board.default_tree().item_shapes(slot, id) {
            let Some(shape) = tile.shape.as_ref() else {
                continue;
            };
            let bb = shape.bounding_box();
            bounds = Some(match bounds {
                Some(existing) => existing.union(&bb),
                None => bb,
            });
        }
    }
    match bounds {
        Some(bb) => IntBox::new(
            bb.left_x().saturating_sub(BOUNDS_MARGIN),
            bb.bottom_y().saturating_sub(BOUNDS_MARGIN),
            bb.right_x().saturating_add(BOUNDS_MARGIN),
            bb.top_y().saturating_add(BOUNDS_MARGIN),
        ),
        None => IntBox::new(-BOUNDS_MARGIN, -BOUNDS_MARGIN, BOUNDS_MARGIN, BOUNDS_MARGIN),
    }
}
