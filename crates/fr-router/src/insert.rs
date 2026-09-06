//! `InsertFoundConnectionAlgo` — the located connection turned into board
//! copper (`pull-tight-insert.md` §4).
//!
//! Transcribed from
//! `/tmp/fr-java-release/src/main/java/app/freerouting/autoroute/InsertFoundConnectionAlgo.java`
//! (514 L): the driver `get_instance` `:42-89` (spec §4.1), `insert_trace`
//! `:94-287` (§4.2), `insert_fanout_micro_neckdown` `:289-333`,
//! `insert_neckdown` / `try_neck_down` `:335-418` (§4.3) and `insert_via`
//! `:424-488` (§4.4). The ripped-connection deletion that precedes it is
//! `AutorouteEngine.autoroute_connection` `:212-241`.
//!
//! # One transaction, not upstream's none
//!
//! **This is the one deliberate behavioural departure of this module.**
//! Upstream has no rollback (spec §4.1, "Failure semantics"): when insertion
//! fails, the ripped items are already gone and the partial traces and vias of
//! the abandoned connection stay on the board, and the batch driver just counts
//! the item as failed. That is a bug the interactive optimizer works around
//! with `generate_snapshot()` (`BatchOptimizer.java:356`), and `fr-board` gives
//! us the cheaper primitive: the whole insertion runs inside one
//! [`Board::begin`]/[`Board::rollback_to`] pair, so a failure leaves the board
//! bit-identical to what it was.
//!
//! `FR_INSERT_FAIL_KEEP_RIPS=1` restores upstream's failure semantics: the
//! ripup stays (victims return to the unrouted queue) and the abandoned
//! attempt's own copper is cleaned as tails. That is the board-evolution
//! channel upstream's late-pass oscillation escapes through — see
//! REPORT-convergence §5.3 — shipped behind the switch pending the sweep
//! verdict. The DRC acceptance guard below is ours alone and rolls back in
//! both modes.
//!
//! The consequence is *fewer* board mutations than the release on a failed
//! connection, never more, and it removes the only source of orphan copper in
//! the pipeline. Batch-level parity is unaffected: the return value is the same
//! ROUTED / INSERT_ERROR distinction the release reports.
//!
//! # M2a scope
//!
//! * shove is off — see [`crate::insert_forced`] for what that does to the
//!   ok-point protocol. The retry ladder above it (window widening, neckdown,
//!   the trichotomy) is ported in full;
//! * `rules.set_pin_edge_to_turn_dist(-1)` (`:108-109`, restored `:281`) is a
//!   documented no-op here: the only reader of that knob is
//!   `PolylineTrace.pull_tight`'s swap/correct-connection-to-pin ladder
//!   (`PolylineTrace.java:856`), which is M2b. The save/restore is noted at its
//!   call site so the seam is visible;
//! * post-insert is `normalize_traces(net)` plus the recorded
//!   [`crate::insert_forced::ChangedArea`]. Upstream runs pull-tight twice —
//!   once inside `insert_forced_trace_polyline` with a keep point, once as
//!   `opt_changed_area` over the region — and so does this port: the first is
//!   step 8 of [`crate::insert_forced::ForcedInserter::insert_forced_trace_polyline`]
//!   (see [`crate::insert_forced::INSERT_PULL_TIGHT_DEFAULT`] for the A/B), the
//!   second is the batch driver's, over the changed area this returns.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use fr_board::board::Board;
use fr_board::connectivity::connection_points;
use fr_board::item::{ItemId, ItemKind};
use fr_geometry::planar::{FloatPoint, IntBox, IntPoint, Point, Polyline};
use fr_rules::board_rules::BoardRules;
use fr_rules::via_info::ViaInfo;

use crate::batch::{remove_tails, ContactScratch, StopConnection};
use crate::ctrl::AutorouteControl;
use crate::insert_forced::{ChangedArea, ForcedInserter};
use crate::locate::{calculate_additional_corner, LocatedConnection, ResultItem};

/// Upstream's neckdown `TOLERANCE` (`InsertFoundConnectionAlgo.java:370`).
pub const NECKDOWN_TOLERANCE: i32 = 2;

/// What one successful insertion changed.
#[derive(Debug, Clone)]
pub struct InsertOutcome {
    /// The per-layer region every mutation was joined into. This is the input
    /// `RoutingBoard.opt_changed_area` takes in M2b (spec §5.2).
    pub changed_area: ChangedArea,
    /// Every item deleted to make room for the new connection: the ripped items
    /// themselves plus the rest of each one's connection
    /// (`AutorouteEngine.java:221-232`). Ascending by id.
    pub ripped_connections: Vec<ItemId>,
    /// The nets those items belonged to, ascending. The batch driver requeues
    /// their connections.
    pub requeue_nets: Vec<i32>,
    /// Whether the insertion left **live copper** on the board: at least one
    /// item created during it — a trace, a via, a connect-to-trace line, a piece
    /// of a normalization split — is still there when it returns.
    ///
    /// Allocating an item is not enough. The insertion deletes its own stubs
    /// (`InsertFoundConnectionAlgo:256-272`) and `connect_to_trace` removes the
    /// tails it exposes (`RoutingBoard.java:1128-1139`), so a degenerate
    /// connection routinely inserts a trace and takes it out again before
    /// returning; on `Issue229` that is 20 of 39 connections, every one of
    /// which the pre-fix binary reported as `ROUTED`.
    ///
    /// **A successful insert that changed nothing is not a route**, and saying
    /// so is the difference between a report and a fiction. It happens: a
    /// located connection of a single corner is "already at the target"
    /// (`InsertFoundConnectionAlgo.java:95-103`) and inserts nothing, and
    /// `connect_to_trace` returns early when the corner is already on the
    /// target trace's polyline (`RoutingBoard.java:1100-1105`) — which is only
    /// a *connection* if the following `normalize_traces` can split that trace
    /// at the point, and it cannot when the trace is `USER_FIXED`
    /// (`PolylineTrace.split:707-709`). On `Issue229`, whose whole `(wiring)`
    /// scope is `(type protect)`, that is every connection on the board: 39
    /// attempts, 39 `ROUTED` results, and a session byte-identical in copper to
    /// the input.
    pub board_changed: bool,
}

/// Why an insertion failed. Every variant leaves the board untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertError {
    /// A layer change had no via mask spanning it
    /// (`InsertFoundConnectionAlgo.java:456-458`).
    NoViaSpan {
        /// Lower layer of the requested span.
        from_layer: i32,
        /// Upper layer of the requested span.
        to_layer: i32,
    },
    /// A via mask spanned the layers but no candidate passed the forced-via
    /// check, or the insertion itself failed (`:459-486`).
    ViaBlocked {
        /// Lower layer of the requested span.
        from_layer: i32,
        /// Upper layer of the requested span.
        to_layer: i32,
        /// Where the via would have gone.
        location: IntPoint,
    },
    /// A trace segment could not be inserted (`:213-253`, the FAIL branch).
    TraceBlocked {
        /// The layer it was on.
        layer: i32,
        /// Index of the corner the insertion stalled before.
        corner_index: usize,
    },
    /// The located connection carried no traces at all.
    NothingToInsert,
}

/// `InsertFoundConnectionAlgo.get_instance` (`:42-89`, spec §4.1) — the whole
/// insertion, in one board transaction.
///
/// On `Ok` the board carries the new connection and the ripped items are gone;
/// on `Err` the board is exactly what it was, down to the search trees and the
/// connectivity index ([`Board::rollback_to`]).
///
/// `bounds` is the board bounding box every clearance check tests containment
/// against — the same value [`crate::engine::AutorouteEngine::bounds`] used, so
/// that a corner the maze considered reachable is not rejected here for a
/// different reason.
pub fn insert_connection(
    board: &mut Board,
    rules: &BoardRules,
    ctrl: &AutorouteControl,
    connection: &LocatedConnection,
    bounds: IntBox,
) -> Result<InsertOutcome, InsertError> {
    insert_connection_with_fail_mode(
        board,
        rules,
        ctrl,
        connection,
        bounds,
        keep_insert_fail_rips(),
    )
}

/// [`insert_connection`] with the failure mode explicit — the env-gated entry
/// point dispatches here, and the tests inject the mode directly so they do
/// not depend on process-global state.
pub fn insert_connection_with_fail_mode(
    board: &mut Board,
    rules: &BoardRules,
    ctrl: &AutorouteControl,
    connection: &LocatedConnection,
    bounds: IntBox,
    keep_fail_rips: bool,
) -> Result<InsertOutcome, InsertError> {
    let mark = board.begin();
    // Item ids are monotone and never reused (`fr_board::store`, `next_id`), so every item this
    // insertion creates has an id at or above this one — and `ItemStore::iter` runs descending,
    // so "did any of them survive" is the first live id it yields. Cheaper and less fragile
    // than threading a counter through every insertion site, of which there are seven.
    let first_free_id = board.store().next_id();
    let mut inserter = ConnectionInserter::new(board, rules, ctrl, bounds);
    match inserter.run(connection) {
        Ok(mut outcome) => {
            let new_items: Vec<ItemId> = inserter
                .forced
                .board
                .store()
                .iter()
                .map(|(id, _)| id)
                .take_while(|id| *id >= first_free_id)
                .collect();
            outcome.board_changed =
                !new_items.is_empty() || inserter.forced.board.touched_since(&mark);

            // DRC symmetric acceptance verification: guarantee zero added violations
            // by construction. If any newly created item has a clearance violation
            // against the board, roll back the transaction clean.
            for id in &new_items {
                if !inserter.forced.check_drc_clear_for_item(*id) {
                    drop(inserter);
                    board.rollback_to(mark);
                    return Err(InsertError::TraceBlocked {
                        layer: connection.target_layer,
                        corner_index: 0,
                    });
                }
            }
            drop(inserter);
            board.commit(mark);
            Ok(outcome)
        }
        Err(error) => {
            drop(inserter);
            if keep_fail_rips {
                // Upstream failure semantics (`InsertFoundConnectionAlgo.java:250-268`
                // + the absence of any undo in the autoroute package): the ripup
                // stays (its victims return to the unrouted queue), and the
                // abandoned attempt's own copper is cleaned up as tails —
                // upstream removes the stubs at the path corners in-insert and
                // the batch driver's end-of-pass `remove_tails`
                // (`BatchAutorouter.java:651-661`) catches the rest. Scoped to
                // the routed net so other nets' copper is untouched.
                board.commit(mark);
                let mut scratch = crate::batch::ContactScratch::default();
                crate::batch::remove_tails(
                    board,
                    ctrl.net_no,
                    crate::batch::StopConnection::None,
                    &mut scratch,
                );
                Err(error)
            } else {
                board.rollback_to(mark);
                Err(error)
            }
        }
    }
}

/// The `FR_INSERT_FAIL_KEEP_RIPS` switch: upstream's non-transactional insert
/// failure. Off (default) keeps this port's transactional rollback; on, a
/// failed insertion keeps the ripup (victims stay torn, returning to the
/// unrouted queue) and cleans the abandoned attempt's own tails, which is the
/// board-evolution channel upstream's late-pass oscillation escapes through
/// (REPORT-convergence §5.3).
fn keep_insert_fail_rips() -> bool {
    static KEEP_RIPS: OnceLock<bool> = OnceLock::new();
    *KEEP_RIPS.get_or_init(|| {
        std::env::var("FR_INSERT_FAIL_KEEP_RIPS")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
            .unwrap_or(false)
    })
}

struct ConnectionInserter<'b> {
    forced: ForcedInserter<'b>,
    first_corner: Option<IntPoint>,
    last_corner: Option<IntPoint>,
}

impl<'b> ConnectionInserter<'b> {
    fn new(
        board: &'b mut Board,
        rules: &'b BoardRules,
        ctrl: &'b AutorouteControl,
        bounds: IntBox,
    ) -> ConnectionInserter<'b> {
        ConnectionInserter {
            forced: ForcedInserter::new(board, rules, ctrl, bounds),
            first_corner: None,
            last_corner: None,
        }
    }

    fn ctrl(&self) -> &AutorouteControl {
        self.forced.ctrl
    }

    fn run(&mut self, connection: &LocatedConnection) -> Result<InsertOutcome, InsertError> {
        // ---- ripup, `AutorouteEngine.java:212-239` -----------------------
        let (ripped_connections, requeue_nets) = self.remove_ripped(connection);

        // ---- the driver, `:46-67` ----------------------------------------
        let mut curr_layer = connection.target_layer;
        for item in &connection.connection_items {
            let Some(first) = item.corners.first().copied() else {
                continue;
            };
            self.insert_via(first, curr_layer, item.layer)?;
            curr_layer = item.layer;
            self.insert_trace(item)?;
        }
        // `:68-70`: the last via, from the last trace's layer to the start
        // item's layer.
        if let Some(last_corner) = self.last_corner {
            self.insert_via(last_corner, curr_layer, connection.start_layer)?;
        }

        // ---- `:71-84`, with v2's null guards -----------------------------
        if let Some(target_item) = connection.target_item {
            if self.is_polyline_trace(target_item) {
                if let Some(first_corner) = self.first_corner {
                    let half_width = self.trace_half_width(connection.start_layer);
                    self.forced
                        .connect_to_trace(first_corner, target_item, half_width);
                }
            }
        }
        if self.is_polyline_trace(connection.start_item) {
            if let Some(last_corner) = self.last_corner {
                let half_width = self.trace_half_width(connection.target_layer);
                self.forced
                    .connect_to_trace(last_corner, connection.start_item, half_width);
            }
        }

        // ---- `:86` -------------------------------------------------------
        let net_no = self.ctrl().net_no;
        self.forced.board.normalize_traces(net_no);

        Ok(InsertOutcome {
            changed_area: self.forced.changed_area.clone(),
            ripped_connections,
            requeue_nets,
            // Filled in by `insert_connection`, which is the only thing that can see the
            // board on both sides of this run.
            board_changed: false,
        })
    }

    /// `AutorouteEngine.autoroute_connection` `:212-239`: delete each ripped
    /// item together with the rest of its connection, then remove the trace
    /// tails the deletion left behind on every net that lost copper.
    ///
    /// The `StopConnectionOption` is `NONE` unless the fanout pre-pass turned
    /// `remove_unconnected_vias` off, in which case it is `FANOUT_VIA`
    /// (`:216-221`).
    fn remove_ripped(&mut self, connection: &LocatedConnection) -> (Vec<ItemId>, Vec<i32>) {
        let stop_at_fanout_via = !self.ctrl().remove_unconnected_vias;
        let mut ripped_connections: BTreeSet<ItemId> = BTreeSet::new();
        let mut changed_nets: BTreeSet<i32> = BTreeSet::new();

        for ripped in &connection.ripped_items {
            let Some(item) = self.forced.board.store().get(ripped.item) else {
                continue;
            };
            for net in item.nets.iter() {
                changed_nets.insert(*net);
            }
            let walk = connection_items(self.forced.board, ripped.item, stop_at_fanout_via);
            if crate::maze::trace::is_enabled() {
                let describe = |id: ItemId| {
                    self.forced
                        .board
                        .store()
                        .get(id)
                        .map(|item| match &item.kind {
                            ItemKind::Via(v) => format!(
                                "{:?} Via@{},{} nets={:?}",
                                id, v.center.x, v.center.y, item.nets
                            ),
                            ItemKind::Trace(t) => {
                                let fc = t.polyline.first_corner().to_float();
                                let lc = t.polyline.last_corner().to_float();
                                format!(
                                    "{:?} Trace@{},{}..{},{} nets={:?}",
                                    id,
                                    fc.x as i32,
                                    fc.y as i32,
                                    lc.x as i32,
                                    lc.y as i32,
                                    item.nets
                                )
                            }
                            ItemKind::Pin(p) => format!(
                                "{id:?} Pin@{},{} nets={:?}",
                                p.center.x, p.center.y, item.nets
                            ),
                            _ => format!("{id:?} other"),
                        })
                };
                let list = walk
                    .iter()
                    .filter_map(|id| describe(*id))
                    .collect::<Vec<_>>();
                crate::maze::trace::conn_landing(&format!(
                    "FRCONN remove_ripped net={} seed={:?} stop_fanout_via={} walk={}",
                    self.ctrl().net_no,
                    ripped.item,
                    stop_at_fanout_via,
                    list.join(" | ")
                ));
            }
            for id in walk {
                ripped_connections.insert(id);
            }
        }

        for id in &ripped_connections {
            // Mark what the removal frees, so the M2b regional pull-tight
            // reaches it (`RoutingBoard.remove_items_and_pull_tight`).
            if let Some(item) = self.forced.board.store().get(*id) {
                let layer_range = item.layer_range();
                if let ItemKind::Trace(trace) = &item.kind {
                    let corners = trace.polyline.corner_approx_arr();
                    for corner in corners {
                        self.forced.changed_area.join_point(corner, trace.layer);
                    }
                } else if let Some(point) = connection_points(item).iter().next() {
                    let point = point.to_float();
                    for layer in layer_range.first..=layer_range.last {
                        self.forced.changed_area.join_point(point, layer);
                    }
                }
            }
            self.forced.board.remove_item(*id);
        }

        let stop = if stop_at_fanout_via {
            StopConnection::FanoutVia
        } else {
            StopConnection::None
        };
        let mut scratch = ContactScratch::new();
        for net in &changed_nets {
            remove_tails(self.forced.board, *net, stop, &mut scratch);
        }

        (
            ripped_connections.into_iter().collect(),
            changed_nets.into_iter().collect(),
        )
    }

    fn is_polyline_trace(&self, id: ItemId) -> bool {
        self.forced
            .board
            .store()
            .get(id)
            .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
    }

    fn trace_half_width(&self, layer: i32) -> i32 {
        let index = (layer.max(0) as usize).min(self.ctrl().trace_half_width.len() - 1);
        self.ctrl().trace_half_width[index]
    }

    // ---- §4.4 ------------------------------------------------------------

    /// `insert_via` (`:424-488`): the cheapest via mask spanning `[from, to]`
    /// that passes the forced-via check, then the insertion.
    fn insert_via(
        &mut self,
        location: IntPoint,
        from_layer: i32,
        to_layer: i32,
    ) -> Result<(), InsertError> {
        if from_layer == to_layer {
            return Ok(());
        }
        let (from_layer, to_layer) = (from_layer.min(to_layer), from_layer.max(to_layer));

        // The rule is scanned cheapest-first, in declaration order (`:442`).
        let candidates: Vec<ViaInfo> = (0..self.ctrl().via_rule.via_count())
            .filter_map(|index| self.ctrl().via_rule.get_via(index).cloned())
            .collect();

        let mut found_suitable_span = false;
        let mut chosen: Option<ViaInfo> = None;
        for candidate in &candidates {
            let padstack = candidate.padstack();
            if padstack.from_layer() as i32 > from_layer || (padstack.to_layer() as i32) < to_layer
            {
                continue;
            }
            found_suitable_span = true;
            if self.forced.check_via(candidate, location) {
                chosen = Some(candidate.clone());
                break;
            }
        }

        let Some(via_info) = chosen else {
            return Err(if found_suitable_span {
                InsertError::ViaBlocked {
                    from_layer,
                    to_layer,
                    location,
                }
            } else {
                InsertError::NoViaSpan {
                    from_layer,
                    to_layer,
                }
            });
        };
        if !self.forced.insert_forced_via(&via_info, location) {
            return Err(InsertError::ViaBlocked {
                from_layer,
                to_layer,
                location,
            });
        }
        Ok(())
    }

    // ---- §4.2 ------------------------------------------------------------

    /// `insert_trace` (`:94-287`) — the incremental window loop and its
    /// ok-point trichotomy.
    fn insert_trace(&mut self, trace: &ResultItem) -> Result<(), InsertError> {
        if trace.corners.len() == 1 {
            // `:95-103` (v2 delta §10.2): a degenerate single-point trace sets
            // *both* corners, so `connect_to_trace` is never called with null.
            let corner = trace.corners[0];
            if self.first_corner.is_none() {
                self.first_corner = Some(corner);
            }
            self.last_corner = Some(corner);
            return Ok(());
        }

        // `:108-109` / `:281`: upstream saves `rules.get_pin_edge_to_turn_dist()`,
        // forces it to `-1` for the duration of the line-by-line insertion, and
        // restores it afterwards.
        //
        // The knob gates exactly one thing — the `swap_connection_to_pin` /
        // `correct_connection_to_pin` ladder at the tail of
        // `PolylineTrace.pull_tight` (`PolylineTrace.java:855-873`, gated on
        // `> 0`). That ladder is **not ported** (see [`crate::pull_tight`]), so
        // the knob still has no reader and forcing it here would be theatre:
        // `rules` is `&BoardRules` on this path, so faking the save/restore
        // would mean widening the borrow of a value nothing consults.
        //
        // The contract is therefore satisfied vacuously, and the *invariant* it
        // protects — that no pull-tight run inside an insertion may re-route a
        // pin exit — holds because the ladder does not exist. Whoever ports the
        // ladder must make this a real save/restore in the same commit; that is
        // the only reason this comment is still here.

        let layer = trace.layer;
        let half_width = self.trace_half_width(layer);
        let (start_pin, end_pin) = self.pick_neckdown_pins(trace);

        let mut result = Ok(());
        let mut from_corner_no = 0usize;
        let mut index = 1usize;
        while index < trace.corners.len() {
            let window = &trace.corners[from_corner_no..=index];
            let polyline = Polyline::from_int_points(window);
            let first = polyline.first_corner();
            let last = polyline.last_corner();
            let ok_point =
                self.forced
                    .insert_forced_trace_polyline(&polyline, half_width, layer, i32::MAX);

            let mut neckdown_inserted = false;
            let mut micro_neckdown_inserted = false;
            if let Some(ok) = ok_point.as_ref() {
                // `:150-152`: neckdown only for a single-segment window.
                if *ok != last && self.ctrl().with_neckdown && window.len() == 2 {
                    if let Some(ok_int) = as_int(ok) {
                        neckdown_inserted =
                            self.insert_neckdown(ok_int, window[1], layer, start_pin, end_pin);
                    }
                }
            }
            // `:153-156` (v2 delta §10.2): the fanout micro-neckdown ladder.
            if !neckdown_inserted
                && ok_point.as_ref() != Some(&last)
                && (self.ctrl().is_fanout || self.ctrl().with_neckdown)
                && window.len() == 2
            {
                micro_neckdown_inserted = self.insert_fanout_micro_neckdown(
                    ok_point.as_ref().and_then(as_int),
                    window[1],
                    layer,
                    start_pin,
                    end_pin,
                );
            }

            if ok_point.as_ref() == Some(&last) || neckdown_inserted || micro_neckdown_inserted {
                // ADVANCE (`:157-178`).
                from_corner_no = index;
            } else if ok_point.as_ref() == Some(&first) && index != trace.corners.len() - 1 {
                // VIOLATION_CORRECTED (`:179-193`). Insertion is checked with
                // clearance compensation while the final geometry may violate
                // without it; widening the window by one corner lets spring-over
                // fix that. The widening happens **once** — `window.len() < 3`
                // — and `index` still advances, exactly as upstream's `for`
                // variable does. Both together bound the loop at `n` iterations
                // and keep `from_corner_no <= index`.
                if from_corner_no > 0 && window.len() < 3 {
                    from_corner_no -= 1;
                }
            } else {
                // FAIL (`:213-253`).
                if crate::maze::trace::is_enabled() {
                    crate::maze::trace::conn_landing(&format!(
                        "FRCONN trace_fail net={} layer={} idx={}/{} from_idx={} ok_point={:?} target={:?} first={:?} window_len={}",
                        self.ctrl().net_no, layer, index, trace.corners.len() - 1, from_corner_no, ok_point, last, first, window.len()
                    ));
                }
                result = Err(InsertError::TraceBlocked {
                    layer,
                    corner_index: index,
                });
                break;
            }
            index += 1;
        }

        // `:256-272` — stub cleanup, unconditional in the release (D5). Runs
        // even after a failure, exactly as upstream does.
        for corner in &trace.corners[..trace.corners.len() - 1] {
            if let Some(stub) = self.forced.get_trace_tail(*corner, layer) {
                self.forced.board.remove_item(stub);
            }
        }

        if self.first_corner.is_none() {
            self.first_corner = Some(trace.corners[0]);
        }
        self.last_corner = Some(trace.corners[trace.corners.len() - 1]);
        result
    }

    /// `:114-133`: the same-net pins sitting exactly at the trace's two end
    /// corners, for the neckdown fallback.
    fn pick_neckdown_pins(&mut self, trace: &ResultItem) -> (Option<ItemId>, Option<ItemId>) {
        if !self.ctrl().with_neckdown && !self.ctrl().is_fanout {
            return (None, None);
        }
        let first = trace.corners[0];
        let last = trace.corners[trace.corners.len() - 1];
        (
            self.pin_at(first, trace.layer),
            self.pin_at(last, trace.layer),
        )
    }

    fn pin_at(&mut self, corner: IntPoint, layer: i32) -> Option<ItemId> {
        let mut picked = Vec::new();
        self.forced
            .board
            .pick_items(&Point::Int(corner), Some(layer), &mut picked);
        let net_no = self.ctrl().net_no;
        picked.into_iter().find(|id| {
            self.forced.board.store().get(*id).is_some_and(|item| {
                matches!(&item.kind, ItemKind::Pin(pin) if pin.center == corner)
                    && item.contains_net(net_no)
            })
        })
    }

    // ---- §4.3 ------------------------------------------------------------

    /// `insert_neckdown` (`:335-347`).
    fn insert_neckdown(
        &mut self,
        from_corner: IntPoint,
        to_corner: IntPoint,
        layer: i32,
        start_pin: Option<ItemId>,
        end_pin: Option<ItemId>,
    ) -> bool {
        if let Some(pin) = start_pin {
            let ok_point = self.try_neck_down(to_corner, from_corner, layer, pin);
            if ok_point == Some(from_corner) {
                return true;
            }
        }
        if let Some(pin) = end_pin {
            let ok_point = self.try_neck_down(from_corner, to_corner, layer, pin);
            return ok_point == Some(to_corner);
        }
        false
    }

    /// `try_neck_down` (`:349-418`).
    ///
    /// Splits `from → to` at the last point that still fits at full width, then
    /// runs the remaining stub into the pin at the pin's reduced neckdown half
    /// width. Returns the point insertion reached, or `None` when the
    /// preconditions do not hold.
    fn try_neck_down(
        &mut self,
        from_corner: IntPoint,
        to_corner: IntPoint,
        layer: i32,
        pin: ItemId,
    ) -> Option<IntPoint> {
        let (pin_center, pin_clearance_class, pin_max_width, neck_down_halfwidth) =
            self.pin_neckdown_geometry(pin, layer)?;

        let clearance = self.forced.rules.clearance_matrix.get_value(
            self.ctrl().trace_clearance_class_no.max(0) as usize,
            pin_clearance_class.max(0) as usize,
            layer.max(0) as usize,
            true,
        ) as f64;
        let pin_neck_down_distance = 2.0 * (0.5 * pin_max_width + clearance);
        if pin_center.distance(&to_corner.to_float()) >= pin_neck_down_distance {
            return None;
        }

        let full_half_width = self.trace_half_width(layer);
        if neck_down_halfwidth >= full_half_width {
            return None;
        }

        let float_from = from_corner.to_float();
        let float_to = to_corner.to_float();
        let tolerance = NECKDOWN_TOLERANCE as f64;

        let ok_length =
            self.forced
                .check_trace_segment(from_corner, to_corner, layer, full_half_width, true);
        if ok_length.is_infinite() {
            // No conflict at all: nothing to neck down (`:376-378`).
            return Some(from_corner);
        }
        let ok_length = ok_length - tolerance;

        let angle_restriction = self.forced.board.angle_restriction();
        let mut neck_down_end_point = from_corner;
        if ok_length > tolerance {
            let float_neck = float_from.change_length(&float_to, ok_length);
            neck_down_end_point = float_neck.round();
            // `:386-391`: bridge the rounding error with knees.
            let horizontal_first =
                (float_from.x - float_neck.x).abs() >= (float_from.y - float_neck.y).abs();
            let add_corner = calculate_additional_corner(
                float_from,
                float_neck,
                horizontal_first,
                angle_restriction,
            )
            .round();
            if self.forced.insert_forced_trace_segment(
                from_corner,
                add_corner,
                full_half_width,
                layer,
            ) != Some(Point::Int(add_corner))
            {
                return Some(from_corner);
            }
            if self.forced.insert_forced_trace_segment(
                add_corner,
                neck_down_end_point,
                full_half_width,
                layer,
            ) != Some(Point::Int(neck_down_end_point))
            {
                return Some(from_corner);
            }
            let add_corner = calculate_additional_corner(
                float_neck,
                float_to,
                !horizontal_first,
                angle_restriction,
            )
            .round();
            if add_corner != to_corner {
                if self.forced.insert_forced_trace_segment(
                    neck_down_end_point,
                    add_corner,
                    full_half_width,
                    layer,
                ) != Some(Point::Int(add_corner))
                {
                    return Some(from_corner);
                }
                neck_down_end_point = add_corner;
            }
        }

        // `:415-417`: the reduced-width stub into the pin.
        self.forced
            .insert_forced_trace_segment(neck_down_end_point, to_corner, neck_down_halfwidth, layer)
            .and_then(|point| as_int(&point))
    }

    /// `insert_fanout_micro_neckdown` (`:289-333`, v2 delta §10.2).
    ///
    /// The candidate ladder is upstream's insertion-ordered, deduplicated set:
    /// the two pins' neckdown half widths, then ¾, ⅗ and ½ of the base — all
    /// **integer** division, each skipped when `<= 0` or `>= base`.
    fn insert_fanout_micro_neckdown(
        &mut self,
        ok_point: Option<IntPoint>,
        target_point: IntPoint,
        layer: i32,
        start_pin: Option<ItemId>,
        end_pin: Option<ItemId>,
    ) -> bool {
        let from_point = ok_point.unwrap_or(target_point);
        if from_point == target_point {
            return false;
        }
        let base_half_width = self.trace_half_width(layer);
        let mut candidates: Vec<i32> = Vec::with_capacity(5);
        let push = |candidates: &mut Vec<i32>, value: i32| {
            if !candidates.contains(&value) {
                candidates.push(value);
            }
        };
        for pin in [start_pin, end_pin].into_iter().flatten() {
            if let Some((_, _, _, neckdown)) = self.pin_neckdown_geometry(pin, layer) {
                push(&mut candidates, neckdown);
            }
        }
        push(&mut candidates, 1.max((base_half_width * 3) / 4));
        push(&mut candidates, 1.max((base_half_width * 3) / 5));
        push(&mut candidates, 1.max(base_half_width / 2));

        for candidate in candidates {
            if candidate <= 0 || candidate >= base_half_width {
                continue;
            }
            if self
                .forced
                .insert_forced_trace_segment(from_point, target_point, candidate, layer)
                == Some(Point::Int(target_point))
            {
                return true;
            }
        }
        false
    }

    /// `(center, clearance class, max pad width, neckdown half width)` of a pin
    /// on `layer`, or `None` when the pin is not on it.
    ///
    /// `Pin.get_trace_neckdown_halfwidth(layer) = max(0.5 * min_width - 1, 1)`
    /// (`board/Pin.java`), and `get_max_width` / `get_min_width` are the pad's
    /// bounding-box extents.
    fn pin_neckdown_geometry(
        &self,
        pin: ItemId,
        layer: i32,
    ) -> Option<(FloatPoint, i32, f64, i32)> {
        let item = self.forced.board.store().get(pin)?;
        let ItemKind::Pin(pin_payload) = &item.kind else {
            return None;
        };
        if !item.is_on_layer(layer) {
            return None;
        }
        let padstack = self.forced.board.padstacks().get(pin_payload.padstack_no)?;
        let pad = padstack.shape_on_layer(layer)?;
        let bb = pad.bounding_box();
        let max_width = bb.max_width();
        let min_width = bb.min_width();
        let neckdown = (0.5 * min_width - 1.0).max(1.0) as i32;
        Some((
            pin_payload.center.to_float(),
            item.clearance_class,
            max_width,
            neckdown,
        ))
    }
}

/// `Item.get_connection_items(stop_option)` (`board/Item.java`): the routable
/// items reachable from `start` by walking **unique** normal contacts, stopping
/// at the first fork or non-routable item on each side.
///
/// This is the ripup unit `fr-board` deliberately left to the router (its
/// module docs, "What lands next"). `stop_at_fanout_via` is upstream's
/// `StopConnectionOption.FANOUT_VIA`, used only by the fanout pre-pass; a
/// fanout via is one whose only other contact in the collected set is a pin.
pub fn connection_items(board: &Board, start: ItemId, stop_at_fanout_via: bool) -> Vec<ItemId> {
    let mut result: BTreeSet<ItemId> = BTreeSet::new();
    let Some(item) = board.store().get(start) else {
        return Vec::new();
    };
    let start_is_trace = matches!(item.kind, ItemKind::Trace(_));
    if item.is_routable() {
        result.insert(start);
    }

    let mut contacts = Vec::new();
    let mut scratch = Vec::new();
    for (contact, contact_point, contact_layer) in normal_contacts(board, start, &mut scratch) {
        if start_is_trace {
            // `Item.java`: only one contact may sit at this location, or the
            // connection forks right at the start.
            board.normal_contacts_at(start, &contact_point, &mut contacts);
            if contacts.len() != 1 {
                continue;
            }
        }
        let mut curr = contact;
        let mut prev_point = contact_point;
        let mut prev_layer = contact_layer;
        let mut guard = board.store().len() + 1;
        loop {
            if guard == 0 {
                break;
            }
            guard -= 1;
            let Some(curr_item) = board.store().get(curr) else {
                break;
            };
            if !curr_item.is_routable() {
                break;
            }
            if stop_at_fanout_via
                && matches!(curr_item.kind, ItemKind::Via(_))
                && is_fanout_via(board, curr, &result)
            {
                break;
            }
            result.insert(curr);

            let mut next: Option<(ItemId, Point, i32)> = None;
            let mut fork_found = false;
            for (candidate, point, layer) in normal_contacts(board, curr, &mut scratch) {
                if layer < 0 {
                    continue;
                }
                if prev_layer != layer || point != prev_point {
                    if next.is_some() {
                        fork_found = true;
                        break;
                    }
                    next = Some((candidate, point, layer));
                }
            }
            let Some((next_item, next_point, next_layer)) = next else {
                break;
            };
            if fork_found {
                break;
            }
            curr = next_item;
            prev_point = next_point;
            prev_layer = next_layer;
        }
    }
    result.into_iter().collect()
}

/// `Item.get_normal_contacts()` flattened to `(contact, unique contact point,
/// first common layer)`.
///
/// A contact with no *unique* contact point is dropped, which is upstream's
/// `normal_contact_point(other) == null` case ("no unique contact point",
/// `Item.java`).
fn normal_contacts(
    board: &Board,
    id: ItemId,
    scratch: &mut Vec<ItemId>,
) -> Vec<(ItemId, Point, i32)> {
    let Some(item) = board.store().get(id) else {
        return Vec::new();
    };
    let points: Vec<Point> = connection_points(item).iter().cloned().collect();
    // Each contact may be reachable at more than one of our connection points
    // (a trace looping back to the same via); such a contact has no unique
    // contact point and upstream skips it.
    let mut hits: BTreeMap<ItemId, Vec<Point>> = BTreeMap::new();
    for point in &points {
        board.normal_contacts_at(id, point, scratch);
        for contact in scratch.iter().copied() {
            hits.entry(contact).or_default().push(point.clone());
        }
    }
    let item_range = item.layer_range();
    hits.into_iter()
        .filter_map(|(contact, points)| {
            if points.len() != 1 {
                return None;
            }
            let other = board.store().get(contact)?;
            let other_range = other.layer_range();
            let first_common = item_range.first.max(other_range.first);
            if first_common > item_range.last.min(other_range.last) {
                return None;
            }
            Some((contact, points.into_iter().next()?, first_common))
        })
        .collect()
}

/// `Item.is_fanout_via(collected)`: a via whose only contacts, besides the ones
/// already collected, are pins.
fn is_fanout_via(board: &Board, via: ItemId, collected: &BTreeSet<ItemId>) -> bool {
    let mut scratch = Vec::new();
    let contacts = normal_contacts(board, via, &mut scratch);
    let mut saw_pin = false;
    for (contact, _, _) in contacts {
        if collected.contains(&contact) {
            continue;
        }
        match board.store().get(contact).map(|item| &item.kind) {
            Some(ItemKind::Pin(_)) => saw_pin = true,
            Some(_) => return false,
            None => {}
        }
    }
    saw_pin
}

fn as_int(point: &Point) -> Option<IntPoint> {
    match point {
        Point::Int(p) => Some(*p),
        Point::Rational(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestFixture, TEST_NET};

    #[test]
    fn an_empty_located_connection_succeeds_without_touching_the_board() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let ctrl = fixture.control();
        let before = fixture.board.store().len();
        let connection = LocatedConnection {
            connection_items: Vec::new(),
            start_item: fixture.pad_ids()[0],
            start_layer: 0,
            target_item: Some(fixture.pad_ids()[1]),
            target_layer: 0,
            ripped_items: Vec::new(),
        };
        let bounds = IntBox::new(-4000, -2500, 4000, 2500);
        let outcome = insert_connection(
            &mut fixture.board,
            &fixture.rules,
            &ctrl,
            &connection,
            bounds,
        )
        .expect("empty connection normalizes and succeeds");
        assert_eq!(fixture.board.store().len(), before);
        assert!(outcome.ripped_connections.is_empty());
    }

    #[test]
    fn connection_items_of_a_lone_trace_is_just_that_trace() {
        let fixture = TestFixture::with_foreign_trace();
        let trace = fixture.trace_id();
        assert_eq!(connection_items(&fixture.board, trace, false), vec![trace]);
        let _ = TEST_NET;
    }
}
