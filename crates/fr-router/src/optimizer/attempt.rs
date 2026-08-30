//! One item's rip-and-reroute attempt, factored out of the optimizer's pass
//! loop so that a *worker* can run it on a board clone.
//!
//! This is `BatchOptimizer.opt_route_item` (`:308-414`) with two things lifted
//! out of `self`:
//!
//! * the numbers the attempt reads — [`AttemptConfig`], everything the pass has
//!   already decided by the time it reaches an item;
//! * what to do with a reroute that improved — [`Disposition`]. The sequential
//!   optimizer keeps it; a T1 worker evaluating a candidate on its own clone
//!   discards it, because the *only* thing a worker produces is a verdict. See
//!   [`crate::optimizer::parallel`] for why that is what makes the parallel
//!   optimizer sound.
//!
//! Nothing here decides anything the release does not; [`attempt_item`] is a
//! transcription and `BatchOptimizer::opt_route_item` is a four-line wrapper
//! around it.

use std::collections::BTreeSet;

use fr_board::board::Board;
use fr_board::item::{ItemId, ItemKind};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::batch::ContactScratch;
use crate::insert::connection_items;
use crate::optimizer::improvement::{ItemRouteResult, LengthMetric};
use crate::optimizer::stats;
use crate::optimizer::{ItemRerouter, RerouteRequest};

/// Everything one attempt reads that the pass has already decided.
///
/// `Copy` except for the two references, and deliberately *not* carrying the
/// board: an [`AttemptConfig`] built once per window is what every worker
/// evaluates against, which is the same thing as saying that a worker's verdict
/// is a pure function of `(board, item, config)`.
#[derive(Debug, Clone, Copy)]
pub struct AttemptConfig<'a> {
    /// The board's rules.
    pub rules: &'a BoardRules,
    /// The pass's settings — already stripped of preferred directions on an
    /// even pass (`BatchAutorouter.java:126-132`).
    pub pass_settings: &'a RouterSettings,
    /// `min_cumulative_trace_length` (`:225`), the `before` side of the length
    /// comparison.
    pub min_cumulative_trace_length: f64,
    /// Which length pair the improvement lattice compares.
    pub length_metric: LengthMetric,
    /// [`crate::optimizer::BatchOptimizer::item_ripup_costs`] for a via.
    pub via_ripup_costs: i32,
    /// ... and for a trace (the `×0.6` branch).
    pub trace_ripup_costs: i32,
    /// `optimizer.maxAutoroutePasses`, default 6.
    pub max_autoroute_passes: i32,
    /// `p_with_preferred_directions`, carried for a scripted rerouter.
    pub with_preferred_directions: bool,
}

impl AttemptConfig<'_> {
    /// The `:366-374` ripup schedule, already resolved into the two values.
    fn ripup_costs(&self, item_is_trace: bool) -> i32 {
        if item_is_trace {
            self.trace_ripup_costs
        } else {
            self.via_ripup_costs
        }
    }
}

/// What [`attempt_item`] does with a reroute that improved the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Commit it — `:397-399`, the sequential optimizer.
    Keep,
    /// Roll it back whatever it did, so the board comes out bit-for-bit as it
    /// went in. A T1 worker's disposition: it is measuring, not routing.
    Discard,
}

/// The buffers one attempt reuses, so a pass over a thousand items allocates
/// nothing.
///
/// One per *thread* — the sequential optimizer owns one, and each T1 worker owns
/// its own.
#[derive(Debug, Default)]
pub struct AttemptScratch {
    /// Shared with the item sweep, which is on the same hot path.
    pub contacts: ContactScratch,
    ripped_items: BTreeSet<ItemId>,
    ripped_connections: BTreeSet<ItemId>,
    contact_ids: Vec<ItemId>,
}

impl AttemptScratch {
    /// Empty buffers.
    pub fn new() -> AttemptScratch {
        AttemptScratch::default()
    }
}

/// `opt_route_item(p_item, p_with_preferred_directions, false)` (`:308-414`).
///
/// Rips `item`'s whole electrical connection out, re-routes it with `rerouter`,
/// and returns the improvement lattice's verdict. Under [`Disposition::Keep`]
/// the board keeps the reroute iff it improved; under [`Disposition::Discard`]
/// the board is always restored, so the call is observably read-only.
///
/// The restore is [`Board::begin`]/[`Board::rollback_to`] either way — the
/// journal transaction of `ARCHITECTURE.md` state mechanism 1, O(items
/// touched), not a board snapshot.
pub fn attempt_item(
    board: &mut Board,
    item: ItemId,
    cfg: &AttemptConfig<'_>,
    rerouter: &mut dyn ItemRerouter,
    scratch: &mut AttemptScratch,
    disposition: Disposition,
) -> ItemRouteResult {
    let clock = crate::optimizer::trace::is_enabled().then(std::time::Instant::now);
    // `:315-321` — measured *before* the ripout.
    let key_before = stats::board_key(board);
    let key_before_ms = clock.map_or(0, |clock| clock.elapsed().as_millis());

    let Some(item_ref) = board.store().get(item) else {
        return ItemRouteResult::not_attempted();
    };
    let nets: Vec<i32> = item_ref.nets.iter().copied().collect();
    let trace_corners = match &item_ref.kind {
        ItemKind::Trace(trace) => Some((trace.first_corner(), trace.last_corner())),
        _ => None,
    };
    let is_trace = trace_corners.is_some();

    // `:327-340` — step 1. The sweep matches connection *endpoints*, so a fork
    // hanging off either end of this trace is not enumerated on its own; rip it
    // with us, but only when the whole contact group is unfixed trace, or we
    // would be promising to restore copper we are not allowed to touch.
    let mut ripped_items = std::mem::take(&mut scratch.ripped_items);
    ripped_items.clear();
    ripped_items.insert(item);
    if let Some((first, last)) = trace_corners {
        for corner in [first, last] {
            scratch.contact_ids.clear();
            board.normal_contacts_at(item, &corner, &mut scratch.contact_ids);
            if contains_only_unfixed_traces(board, &scratch.contact_ids) {
                ripped_items.extend(scratch.contact_ids.iter().copied());
            }
        }
    }

    // `:342-346` — step 2, `StopConnectionOption.NONE`: the entire electrical
    // connection, fanout vias included.
    let mut ripped_connections = std::mem::take(&mut scratch.ripped_connections);
    ripped_connections.clear();
    for member in ripped_items.iter().copied() {
        ripped_connections.extend(connection_items(board, member, false));
    }

    // `:348-354` — step 3. One user-fixed member and the whole item is skipped:
    // a partial ripout would leave the connection unroutable.
    let blocked = ripped_connections.iter().copied().any(|id| {
        board
            .store()
            .get(id)
            .is_some_and(fr_board::item::Item::is_user_fixed)
    });
    if blocked {
        scratch.ripped_items = ripped_items;
        scratch.ripped_connections = ripped_connections;
        return ItemRouteResult::not_attempted();
    }

    // `:356-357` — step 4. The undo point.
    let mark = board.begin();

    // `:360-363` — `remove_items` skips what it may not delete rather than
    // failing (`BasicBoard.java:722-733`); `remove_item` already makes the
    // `isDeletionForbidden` half of that check.
    for id in ripped_connections.iter().copied() {
        board.remove_item(id);
    }
    for net_no in nets.iter().copied() {
        board.combine_traces(net_no);
    }

    let request = RerouteRequest {
        ripup_costs: cfg.ripup_costs(is_trace),
        max_passes: cfg.max_autoroute_passes,
        with_preferred_directions: cfg.with_preferred_directions,
    };
    // `:376-378`.
    let reroute_started = clock.map(|_| std::time::Instant::now());
    rerouter.reroute(board, cfg.rules, cfg.pass_settings, &request);
    let reroute_ms = reroute_started.map_or(0, |started| started.elapsed().as_millis());

    // `:381-393`.
    let key_after_started = clock.map(|_| std::time::Instant::now());
    let key_after = stats::board_key(board);
    let key_after_ms = key_after_started.map_or(0, |started| started.elapsed().as_millis());
    let result = ItemRouteResult::new(
        &key_before,
        &key_after,
        cfg.min_cumulative_trace_length,
        cfg.length_metric,
    );

    // `:397-399` / `:405-411`.
    if result.improved() && disposition == Disposition::Keep {
        board.commit(mark);
    } else {
        board.rollback_to(mark);
    }

    if clock.is_some() {
        crate::optimizer::trace::line(&format!(
            "item, item={:?}, kind={}, ripped={}, key_before_ms={key_before_ms}, reroute_ms={reroute_ms}, key_after_ms={key_after_ms}, improved={}",
            item,
            if is_trace { "trace" } else { "drill" },
            ripped_connections.len(),
            result.improved(),
        ));
    }
    scratch.ripped_items = ripped_items;
    scratch.ripped_connections = ripped_connections;
    result
}

/// The `key_after` of a kept attempt, for the caller's
/// `min_cumulative_trace_length` update. Cheaper than threading it out of
/// [`attempt_item`], because the caller only needs it on the improving branch.
pub fn weighted_trace_length(board: &mut Board) -> f64 {
    stats::board_key(board).weighted_trace_length
}

/// `contains_only_unfixed_traces` (`:54-61`). An empty group is vacuously true,
/// as upstream's loop over an empty collection is.
fn contains_only_unfixed_traces(board: &Board, items: &[ItemId]) -> bool {
    items.iter().copied().all(|id| {
        board
            .store()
            .get(id)
            .is_some_and(|item| !item.is_user_fixed() && matches!(item.kind, ItemKind::Trace(_)))
    })
}
