//! The batch driver: work-list selection, one autoroute pass, tail cleanup.
//!
//! Ported from the **2.3.0 release** `autoroute/BatchAutorouter.java`
//! (`/tmp/fr-java-release`), whose flat `autoroute/` package is the authority —
//! the `autoroute/pipeline/` classes the spec body describes are not in the jar
//! (`batch-orchestration.md` conformance appendix, headline correction).
//!
//! | this module | upstream |
//! |---|---|
//! | [`autoroute_items`] | `BatchAutorouter.getAutorouteItems` (`:250-306`) |
//! | [`autoroute_item`] | `BatchAutorouter.autoroute_item` (`:1256-1366`) + `AutorouteEngine.autoroute_connection` (`:127-260`) |
//! | [`BatchRouter::autoroute_pass`] | `BatchAutorouter.autoroute_pass` (`:431-694`) |
//! | [`remove_tails`] | `RoutingBoard.remove_trace_tails` (`:1164-1211`) |
//! | [`is_tail`] | `Trace.is_tail` / `Via.is_tail` |
//! | [`is_fanout_via`] | `Item.is_fanout_via` (`:1251-1282`) |
//! | [`schedule`] | `BatchAutorouter.runBatchLoop` (`:757-1188`) — pass scheduling, every stop rule |
//! | [`history`] | `autoroute/BoardHistory.java` — the score-ranked snapshot store |
//! | [`scoring`] | `BoardStatistics.getNormalizedScore`, through `fr_check` |
//! | [`budget`] | the per-connection `TimeLimit` (`:1311-1313`) and ARCHITECTURE's effort ceiling |
//!
//! Corner placement and insertion are **not** here: this module hands the found
//! connection to [`crate::locate`] and [`crate::insert`], the port of
//! `LocateFoundConnectionAlgo` / `InsertFoundConnectionAlgo`.
//!
//! # Scope
//!
//! * **The pass is here; the schedule is in [`schedule`].** [`BatchRouter::run`]
//!   is a thin adapter: it resolves the settings a run needs and hands itself to
//!   [`schedule::BatchLoop`] as a [`schedule::PassDriver`]. Everything the
//!   release does *between* passes — the [`history::BoardSnapshotStore`]
//!   snapshots, the pass ≥ 8 restore check, the two stagnation trackers, the
//!   one-time fanout recovery, the final best-board restore
//!   (`batch-orchestration.md` §5.2 and the conformance appendix's rows 59-69) —
//!   lives there.
//! * **Ripped connections are requeued inside the pass at `-mp 1` only.**
//!   Upstream re-derives the work list next pass instead; with `max_passes = 1`
//!   there is no next pass. The switch is [`schedule::RequeuePolicy`], declared
//!   in `docs/PARITY-DIVERGENCES.md`.
//! * The per-connection pull-tight (`opt_changed_area`, `:1327-1333`) runs, over
//!   [`crate::insert::InsertOutcome::changed_area`] — see [`crate::pull_tight`]
//!   for what of the family is ported and what is not. Its stall guard is
//!   upstream's `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` (1000 ms) in the default
//!   mode and the round-counted equivalent under `--deterministic`, which is
//!   [`crate::pull_tight::StallGuard`] and the same duality [`budget`] applies
//!   to the maze.
//! * The per-connection budget ([`budget`]) is armed around every connection,
//!   handed to the engine the maze polls, and accounted for in
//!   [`PassOutcome`]; see that module.
//!
//! # The routing sequence, per connection
//!
//! ```no_run
//! # use fr_board::board::Board;
//! # use fr_rules::board_rules::BoardRules;
//! # use fr_rules::router_settings::RouterSettings;
//! use fr_router::batch::{BatchRouter, ContactScratch};
//!
//! # fn drive(board: &mut Board, rules: &BoardRules, settings: &RouterSettings) {
//! let mut router = BatchRouter::new(rules, settings);
//! let outcome = router.run(board, 1);
//! println!("{} incomplete", outcome.incomplete_count);
//! # let _: ContactScratch = ContactScratch::default();
//! # }
//! ```

use std::collections::BTreeSet;

use fr_board::board::Board;
use fr_board::connectivity;
use fr_board::item::{FixedState, Item, ItemId, ItemKind};
use fr_board::store::StoreDelta;
use fr_geometry::planar::{IntPoint, Point};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::ctrl::AutorouteControl;
use crate::engine::AutorouteEngine;
use crate::maze::search::MazeSearch;
use crate::pull_tight::{PullTightAlgo, StallGuard, TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS};

pub mod budget;
pub mod history;
pub mod parallel_conn;
pub mod portfolio;
pub mod schedule;
pub mod scoring;
pub mod threads;
pub(crate) mod trace;

use crate::batch::budget::{BudgetMode, BudgetPolicy, ConnectionClock};
use crate::batch::schedule::{BatchLoop, LoopConfig, PassContext, PassDriver, RequeuePolicy};
use crate::batch::scoring::CheckScorer;

pub use crate::batch::schedule::{
    arm_job_deadline, job_budget_spent, job_stop_requested, job_tail_reserve, request_job_stop,
    reserve_job_tail, JobDeadline, RunOutcome, StopCause, StopReason,
};

/// `Item.PROTECT_FANOUT_LENGTH` (`Item.java:39`): a contact trace longer than
/// this many half widths does not make its via a fanout via.
pub const PROTECT_FANOUT_LENGTH: f64 = 400.0;

/// `BatchAutorouter.start_ripup_costs` default (`DefaultSettings.java:147`)
/// *(jar-verified)*. Pass `n` routes at `START_RIPUP_COSTS * n`.
pub const START_RIPUP_COSTS: i32 = 100;

/// `RoutingBoard.remove_trace_tails`'s `StopConnectionOption`
/// (`Item.java:1339-1341`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopConnection {
    /// Walk through every via.
    None,
    /// Stop at a fanout via, and never delete one.
    FanoutVia,
    /// Stop at any via.
    Via,
}

/// `AutorouteAttemptState` (`autoroute/AutorouteAttemptState.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    /// A connection was found and inserted.
    Routed,
    /// The item's whole net is already in one piece.
    NoUnconnectedNets,
    /// The item is a single-layer pin whose connected set left its layer, or a
    /// precondition of the attempt failed without it being a routing failure.
    AlreadyConnected,
    /// The item's connected set already contains a copper pour.
    ConnectedToPlane,
    /// The maze found nothing.
    NotRouted,
    /// The maze found a path the board refused to take.
    InsertError,
}

impl AttemptState {
    /// `BatchAutorouter.autoroute_pass:607-631` counts these as `skipped`,
    /// neither routed nor failed.
    pub fn is_skip(self) -> bool {
        matches!(
            self,
            AttemptState::AlreadyConnected
                | AttemptState::NoUnconnectedNets
                | AttemptState::ConnectedToPlane
        )
    }
}

/// What one [`autoroute_item`] call did to the board.
#[derive(Debug, Clone, Default)]
pub struct AttemptOutcome {
    /// The classification the pass counts on.
    pub state: Option<AttemptState>,
    /// The items the found path claimed, ascending — upstream's
    /// `ripped_item_list`, which the pass counts.
    pub ripped: Vec<ItemId>,
    /// Everything the insert actually deleted: each ripped item plus the rest
    /// of its connection (`AutorouteEngine.java:221-232`), ascending.
    pub ripped_connections: Vec<ItemId>,
    /// Nets whose wiring the ripup touched, ascending — the requeue set.
    pub changed_nets: Vec<i32>,
}

impl AttemptOutcome {
    /// An outcome that only classifies, with no board change.
    pub fn of_state(state: AttemptState) -> AttemptOutcome {
        AttemptOutcome::of(state)
    }

    fn of(state: AttemptState) -> AttemptOutcome {
        AttemptOutcome {
            state: Some(state),
            ..AttemptOutcome::default()
        }
    }

    /// The state, defaulting to [`AttemptState::NotRouted`].
    pub fn state(&self) -> AttemptState {
        self.state.unwrap_or(AttemptState::NotRouted)
    }
}

/// One entry of the pass work list: an item and the net of it to route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkItem {
    /// The seed item — a pin, a pour, or a fixed trace end.
    pub item: ItemId,
    /// Which of its nets this entry routes.
    pub net_no: i32,
}

/// Reusable buffers for the contact walks, so a pass allocates once.
///
/// ARCHITECTURE "no hot-loop allocation": every contact query in this module
/// borrows its scratch from the caller.
#[derive(Debug, Default)]
pub struct ContactScratch {
    picked: Vec<ItemId>,
    contacts: Vec<ItemId>,
    queue: Vec<ItemId>,
}

impl ContactScratch {
    /// An empty scratch.
    pub fn new() -> ContactScratch {
        ContactScratch::default()
    }
}

// ---------------------------------------------------------------------------
// contacts
// ---------------------------------------------------------------------------

/// Upstream `Item.get_normal_contacts()` — every item sharing a net, a layer
/// and a connection point with `id`, ascending, deduplicated.
pub fn normal_contacts(
    board: &Board,
    id: ItemId,
    scratch: &mut ContactScratch,
    out: &mut Vec<ItemId>,
) {
    out.clear();
    let Some(item) = board.store().get(id) else {
        return;
    };
    match &item.kind {
        ItemKind::ConductionArea(_) => {
            for &net_no in &item.nets {
                for &other_id in board.store().net_items(net_no) {
                    if other_id == id {
                        continue;
                    }
                    let Some(other) = board.store().get(other_id) else {
                        continue;
                    };
                    if !connectivity::shares_layer(item, other) {
                        continue;
                    }
                    let points = connectivity::connection_points(other);
                    if points.iter().any(|p| connectivity::touches_at(item, p))
                        && !out.contains(&other_id)
                    {
                        out.push(other_id);
                    }
                }
            }
        }
        _ => {
            let points = connectivity::connection_points(item);
            for point in points.iter() {
                let mut picked = std::mem::take(&mut scratch.picked);
                board.normal_contacts_at(id, point, &mut picked);
                for contact in picked.iter().copied() {
                    if !out.contains(&contact) {
                        out.push(contact);
                    }
                }
                picked.clear();
                scratch.picked = picked;
            }
        }
    }
    out.sort_unstable();
}

/// The unique connection point shared by `a` and `b`, upstream's
/// `Item.normal_contact_point` — `None` when there is none or more than one.
pub fn normal_contact_point(board: &Board, a: ItemId, b: ItemId) -> Option<Point> {
    let store = board.store();
    let (first, second) = (store.get(a)?, store.get(b)?);
    let mut found: Option<Point> = None;
    for point in connectivity::connection_points(first).iter() {
        if !connectivity::touches_at(second, point) {
            continue;
        }
        match &found {
            Some(existing) if existing == point => {}
            Some(_) => return None,
            None => found = Some(point.clone()),
        }
    }
    found
}

/// Upstream `Item.first_common_layer`, or `None` when the spans are disjoint.
pub fn first_common_layer(board: &Board, a: ItemId, b: ItemId) -> Option<i32> {
    let store = board.store();
    let (a, b) = (store.get(a)?.layer_range(), store.get(b)?.layer_range());
    let first = a.first.max(b.first);
    if first <= a.last.min(b.last) {
        Some(first)
    } else {
        None
    }
}

/// The connected set of `seed` within `net_no`, ascending —
/// upstream's `Item.get_connected_set(net)` as a contact-graph flood fill.
pub fn connected_set(
    board: &Board,
    net_no: i32,
    seed: ItemId,
    scratch: &mut ContactScratch,
) -> Vec<ItemId> {
    let mut result = Vec::new();
    if net_no <= 0 || board.store().get(seed).is_none() {
        return result;
    }
    let mut seen = BTreeSet::new();
    seen.insert(seed);
    scratch.queue.clear();
    scratch.queue.push(seed);
    while let Some(id) = scratch.queue.pop() {
        result.push(id);
        let mut contacts = std::mem::take(&mut scratch.contacts);
        normal_contacts(board, id, scratch, &mut contacts);
        for contact in contacts.iter().copied() {
            let on_net = board
                .store()
                .get(contact)
                .is_some_and(|item| item.contains_net(net_no));
            if on_net && seen.insert(contact) {
                scratch.queue.push(contact);
            }
        }
        contacts.clear();
        scratch.contacts = contacts;
    }
    result.sort_unstable();
    result
}

/// The net's items that `connected` does not contain, ascending —
/// upstream's `Item.get_unconnected_set(net)`.
pub fn unconnected_set(board: &Board, net_no: i32, connected: &[ItemId]) -> Vec<ItemId> {
    board
        .store()
        .net_items(net_no)
        .iter()
        .copied()
        .filter(|id| connected.binary_search(id).is_err())
        .collect()
}

/// Upstream `Trace.is_tail` (`Trace.java:142-149`) and `Via.is_tail`
/// (`Via.java:149-165`); every other class is never a tail.
pub fn is_tail(board: &Board, id: ItemId, scratch: &mut ContactScratch) -> bool {
    let Some(item) = board.store().get(id) else {
        return false;
    };
    match &item.kind {
        ItemKind::Trace(trace) => {
            let mut picked = std::mem::take(&mut scratch.picked);
            board.normal_contacts_at(id, &trace.first_corner(), &mut picked);
            let start_empty = picked.is_empty();
            board.normal_contacts_at(id, &trace.last_corner(), &mut picked);
            let end_empty = picked.is_empty();
            picked.clear();
            scratch.picked = picked;
            start_empty || end_empty
        }
        ItemKind::Via(_) => {
            let mut contacts = std::mem::take(&mut scratch.contacts);
            normal_contacts(board, id, scratch, &mut contacts);
            // `Via.is_tail`: one contact or none is a tail; several contacts
            // are a tail only when they all live on the same layer span.
            let tail = match contacts.first() {
                None => true,
                Some(first) => {
                    let span = board.store().get(*first).map(Item::layer_range);
                    contacts.len() <= 1
                        || contacts
                            .iter()
                            .all(|other| board.store().get(*other).map(Item::layer_range) == span)
                }
            };
            contacts.clear();
            scratch.contacts = contacts;
            tail
        }
        _ => false,
    }
}

/// Upstream `Item.is_fanout_via` (`Item.java:1251-1282`): a via touched by a
/// lone SMD pad, directly or through a short trace.
pub fn is_fanout_via(
    board: &Board,
    id: ItemId,
    ignore: &[ItemId],
    scratch: &mut ContactScratch,
) -> bool {
    let mut contacts = std::mem::take(&mut scratch.contacts);
    normal_contacts(board, id, scratch, &mut contacts);
    let mut result = false;
    for contact in contacts.iter().copied() {
        if is_lone_smd_pin(board, contact, scratch) {
            result = true;
            break;
        }
        let Some(item) = board.store().get(contact) else {
            continue;
        };
        let ItemKind::Trace(trace) = &item.kind else {
            continue;
        };
        if ignore.contains(&contact) {
            continue;
        }
        if trace.polyline.length_total() >= PROTECT_FANOUT_LENGTH * f64::from(trace.half_width) {
            continue;
        }
        let mut trace_contacts = Vec::new();
        normal_contacts(board, contact, scratch, &mut trace_contacts);
        for other in trace_contacts.iter().copied() {
            if is_lone_smd_pin(board, other, scratch) {
                result = true;
                break;
            }
            // Shove-fixed two-corner exit traces of SMD pins count too.
            let is_exit_trace = board.store().get(other).is_some_and(|item| {
                item.fixed_state == FixedState::ShoveFixed
                    && matches!(&item.kind, ItemKind::Trace(t) if t.corner_count() == 2)
            });
            if is_exit_trace {
                result = true;
                break;
            }
        }
        if result {
            break;
        }
    }
    contacts.clear();
    scratch.contacts = contacts;
    result
}

fn is_lone_smd_pin(board: &Board, id: ItemId, scratch: &mut ContactScratch) -> bool {
    let Some(item) = board.store().get(id) else {
        return false;
    };
    if !matches!(item.kind, ItemKind::Pin(_)) || !item.layer_range().is_single() {
        return false;
    }
    let mut contacts = Vec::new();
    normal_contacts(board, id, scratch, &mut contacts);
    contacts.len() <= 1
}

/// Upstream `RoutingBoard.remove_trace_tails(net, stop)`
/// (`RoutingBoard.java:1164-1211`). `net_no <= 0` means every net.
///
/// Returns whether anything was removed.
pub fn remove_tails(
    board: &mut Board,
    net_no: i32,
    stop: StopConnection,
    scratch: &mut ContactScratch,
) -> bool {
    let mut stubs = Vec::new();
    for (id, item) in board.store().iter() {
        if !item.is_routable() || item.nets.len() != 1 {
            continue;
        }
        if net_no > 0 && !item.contains_net(net_no) {
            continue;
        }
        if !is_tail(board, id, scratch) {
            continue;
        }
        if matches!(item.kind, ItemKind::Via(_)) {
            match stop {
                StopConnection::Via => continue,
                StopConnection::FanoutVia if is_fanout_via(board, id, &[], scratch) => continue,
                _ => {}
            }
        }
        stubs.push(id);
    }
    stubs.sort_unstable();

    let mut doomed: BTreeSet<ItemId> = BTreeSet::new();
    for stub in stubs.iter().copied() {
        let mut contacts = Vec::new();
        normal_contacts(board, stub, scratch, &mut contacts);
        if contacts.len() == 1 {
            let walk = crate::insert::connection_items(
                board,
                stub,
                matches!(stop, StopConnection::FanoutVia),
            );
            doomed.extend(walk.iter().copied());
        } else {
            // A via connected on one layer to several traces is not a stub
            // beyond itself.
            doomed.insert(stub);
        }
    }
    if doomed.is_empty() {
        return false;
    }
    for id in doomed {
        if !board.is_deletion_forbidden(id) {
            board.remove_item(id);
        }
    }
    // `:1209` — the same net filter, normalised: `combine_traces` reads any
    // negative number as "every net", but `0` as the (empty) net zero.
    board.combine_traces(if net_no > 0 { net_no } else { -1 });
    true
}

// ---------------------------------------------------------------------------
// work-list selection
// ---------------------------------------------------------------------------

/// Upstream `BatchAutorouter.getAutorouteItems` (`:250-306`).
///
/// The scan runs `ItemStore::iter`, i.e. **descending id** — upstream's
/// `UndoableObjects` order for every board scan
/// (`fr_board::store::ItemStore::iter`, and `Item.compareTo` is
/// `other.id - this.id`). The order *is* the pass ordering; nothing else sorts
/// the work list (the release's airline sort is commented out at `:479-481`).
pub fn autoroute_items(
    board: &Board,
    rules: &BoardRules,
    scratch: &mut ContactScratch,
) -> Vec<WorkItem> {
    let mut work = Vec::new();
    let mut handled: BTreeSet<ItemId> = BTreeSet::new();
    let ids: Vec<ItemId> = board
        .store()
        .iter()
        .filter(|(_, item)| item.is_connectable() && !item.is_routable())
        .map(|(id, _)| id)
        .collect();
    for id in ids {
        if handled.contains(&id) {
            continue;
        }
        let nets: Vec<i32> = match board.store().get(id) {
            Some(item) => item.nets.iter().copied().collect(),
            None => continue,
        };
        for net_no in nets {
            if net_no <= 0 {
                continue;
            }
            let connected = connected_set(board, net_no, id, scratch);
            for member in connected.iter().copied() {
                if board.store().get(member).map(|i| i.nets.len()) == Some(1) {
                    handled.insert(member);
                }
            }
            let net_item_count = board.store().net_items(net_no).len();
            if connected.len() >= net_item_count {
                continue;
            }
            // `:288-294`: on a plane net, an item already joined to the pour
            // would answer CONNECTED_TO_PLANE, so it is dropped here instead.
            if rules
                .nets
                .get(net_no)
                .is_some_and(fr_rules::net::Net::contains_plane)
                && connected.iter().any(|member| {
                    matches!(
                        board.store().get(*member).map(|i| &i.kind),
                        Some(ItemKind::ConductionArea(_))
                    )
                })
            {
                continue;
            }
            work.push(WorkItem { item: id, net_no });
        }
    }
    work
}

/// The ratsnest incomplete count: `Σ over nets max(0, components - 1)`.
///
/// `BatchAutorouter.calculateIncompleteCount` (`:1760-1764`) asks the DRC for
/// it; here it is [`Board::net_component_count`], the same quantity over the
/// same contact graph.
pub fn incomplete_count(board: &mut Board) -> usize {
    let nets: Vec<i32> = board.store().nets().map(|(net, _)| net).collect();
    nets.into_iter()
        .map(|net| board.net_component_count(net).saturating_sub(1))
        .sum()
}

// ---------------------------------------------------------------------------
// one connection
// ---------------------------------------------------------------------------

/// The `FRLAB conn`-shaped record for the settle-diff harness, on the
/// `FR_CONN_TRACE` sink. Field-identical to the lab jar's line
/// (`app.freerouting.frlab.FrLab`): the cross-implementation match key is
/// `(pass, net, src anchor)` — item ids are not shared between the two
/// implementations, pins/vias/traces anchor at centre/first-corner, which are
/// board coordinates and comparable.
fn emit_conn_landing(
    board: &Board,
    pass_no: i32,
    entry: WorkItem,
    first_new_id: ItemId,
    expansions_before: u64,
    attempt: &AttemptOutcome,
) {
    let (src_type, anchor) = match board.store().get(entry.item).map(|item| &item.kind) {
        Some(ItemKind::Pin(pin)) => ("Pin", pin.center),
        Some(ItemKind::Via(via)) => ("Via", via.center),
        Some(ItemKind::Trace(trace)) => {
            ("PolylineTrace", point_int(&trace.polyline.first_corner()))
        }
        _ => ("Item", IntPoint::new(0, 0)),
    };
    // The landing: every item the attempt inserted, pull-tight replacements
    // included — the store iterates descending id, so the new ids are a prefix.
    let mut paths = String::new();
    let mut vias = String::new();
    let mut trace_count = 0u32;
    let mut via_count = 0u32;
    for (id, item) in board.store().iter() {
        if id < first_new_id {
            break;
        }
        match &item.kind {
            ItemKind::Trace(trace) => {
                if trace_count > 0 {
                    paths.push('|');
                }
                trace_count += 1;
                for (index, corner) in trace.polyline.corner_arr().iter().enumerate() {
                    if index > 0 {
                        paths.push(';');
                    }
                    let p = point_int(corner);
                    paths.push_str(&format!("{},{}", p.x, p.y));
                }
            }
            ItemKind::Via(via) => {
                if via_count > 0 {
                    vias.push('|');
                }
                via_count += 1;
                vias.push_str(&format!(
                    "{},{},{}-{}",
                    via.center.x, via.center.y, via.layers.first, via.layers.last
                ));
            }
            _ => {}
        }
    }
    let state = match attempt.state() {
        AttemptState::Routed => "ROUTED",
        AttemptState::NoUnconnectedNets => "NO_UNCONNECTED_NETS",
        AttemptState::AlreadyConnected => "ALREADY_CONNECTED",
        AttemptState::ConnectedToPlane => "CONNECTED_TO_PLANE",
        AttemptState::NotRouted => "FAILED",
        AttemptState::InsertError => "INSERT_ERROR",
    };
    let expansions = fr_metrics::counters::value(fr_metrics::counters::Counter::Expansions)
        .saturating_sub(expansions_before);
    crate::maze::trace::conn_landing(&format!(
        "FRCONN conn pass={pass_no} net={} src={src_type}@{},{} item={} exp={expansions} \
         ok={state} traces={trace_count} path={paths} vias={via_count}:{vias}",
        entry.net_no, anchor.x, anchor.y, entry.item,
    ));
}

fn point_int(point: &Point) -> IntPoint {
    match point {
        Point::Int(p) => *p,
        other => other.to_float().round(),
    }
}

/// Up to four `T@x,y` anchors of an item set, for the `FRCONN aim` diagnostic.
fn aim_anchors(board: &Board, items: &[ItemId]) -> String {
    let mut out = String::new();
    for (index, id) in items.iter().take(4).enumerate() {
        if index > 0 {
            out.push(';');
        }
        let (kind, p) = match board.store().get(*id).map(|item| &item.kind) {
            Some(ItemKind::Pin(pin)) => ("Pin", pin.center),
            Some(ItemKind::Via(via)) => ("Via", via.center),
            Some(ItemKind::Trace(trace)) => ("Tr", point_int(&trace.polyline.first_corner())),
            _ => ("?", IntPoint::new(0, 0)),
        };
        out.push_str(&format!("{kind}@{},{}", p.x, p.y));
    }
    if items.len() > 4 {
        out.push_str(&format!(";+{}", items.len() - 4));
    }
    out
}

/// Routes one net of one item — `BatchAutorouter.autoroute_item` (`:1256-1366`)
/// plus `AutorouteEngine.autoroute_connection` (`:127-260`), which the release
/// splits only because the engine owns the room database.
///
/// `ripup_costs` is the pass's `start_ripup_costs * pass_no`; the caller owns
/// the escalation (conformance D5 — `AutorouteControl`'s own default is dead).
///
/// `clock` is this connection's allowance — upstream's `TimeLimit`, built at
/// `:1311-1313` and handed to `board.init_autoroute` at `:1315-1318`. `None`
/// routes unbounded, which is upstream's `null`.
#[allow(clippy::too_many_arguments)]
pub fn autoroute_item(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    item: ItemId,
    net_no: i32,
    ripup_costs: i32,
    remove_unconnected_vias: bool,
    clock: Option<&mut ConnectionClock>,
    scratch: &mut ContactScratch,
) -> AttemptOutcome {
    let contains_plane = rules
        .nets
        .get(net_no)
        .is_some_and(fr_rules::net::Net::contains_plane);

    let connected = connected_set(board, net_no, item, scratch);
    let unconnected = unconnected_set(board, net_no, &connected);
    if unconnected.is_empty() {
        return AttemptOutcome::of(AttemptState::NoUnconnectedNets);
    }
    if contains_plane
        && connected.iter().any(|member| {
            matches!(
                board.store().get(*member).map(|i| &i.kind),
                Some(ItemKind::ConductionArea(_))
            )
        })
    {
        return AttemptOutcome::of(AttemptState::ConnectedToPlane);
    }
    // `:1289-1305`: a plane net routes out of what already touches the pour.
    let (start, destination) = if contains_plane {
        (connected, unconnected)
    } else {
        (unconnected, connected)
    };
    if crate::maze::trace::is_enabled() {
        crate::maze::trace::conn_landing(&format!(
            "FRCONN aim net={} start={} dest={}",
            net_no,
            aim_anchors(board, &start),
            aim_anchors(board, &destination),
        ));
    }

    let via_costs = if contains_plane {
        settings.scoring.plane_via_costs()
    } else {
        settings.scoring.via_costs()
    };
    let mut ctrl = AutorouteControl::for_net_with_costs(board, rules, settings, net_no, via_costs);
    ctrl.ripup_allowed = true;
    ctrl.ripup_costs = ripup_costs;
    ctrl.remove_unconnected_vias = remove_unconnected_vias;

    route_with_control(board, rules, ctrl, &start, &destination, clock)
}

/// The engine → locate → insert half of one connection, with the control
/// already built. The fanout stage enters here with its own control.
///
/// This is `AutorouteEngine.autoroute_connection` (`:127-260`), in the order
/// the release runs it: search, locate **while the room database is still
/// alive**, then release the engine's borrow of the board and insert. The
/// ripped connections travel inside the located connection and are deleted by
/// [`crate::insert::insert_connection`] in the same board transaction, so a
/// failed insert leaves the ripped copper in place (`:213-246`).
///
/// `clock` is the connection's allowance; it is armed on the engine, which is
/// where `RoutingBoard.init_autoroute` puts it (`AutorouteEngine.java:119`) and
/// where the maze loop polls it (`MazeSearchAlgo.java:258`). It stays borrowed
/// so the caller — the pass — can read the usage back after the search, insert
/// and pull-tight it also covers.
pub fn route_with_control(
    board: &mut Board,
    rules: &BoardRules,
    ctrl: AutorouteControl,
    start: &[ItemId],
    destination: &[ItemId],
    clock: Option<&mut ConnectionClock>,
) -> AttemptOutcome {
    board.ensure_compensated_tree(ctrl.trace_clearance_class_no);
    // The connection's own clock says which currency this run spends, and the
    // pull-tight below is inside the window it covers ("armed around the *whole*
    // attempt — search, insert and the per-connection pull-tight",
    // [`budget`]). Read before the clock is lent to the engine.
    let budget_mode = clock
        .as_deref()
        .map_or(BudgetMode::default(), ConnectionClock::mode);
    let (located, bounds) = {
        let mut engine = AutorouteEngine::new(board, rules, ctrl.clone());
        if let Some(clock) = clock {
            engine.set_clock(clock);
        }
        let bounds = engine.bounds();
        let connection = {
            let Ok(mut search) = MazeSearch::get_instance(&mut engine, start, destination) else {
                return AttemptOutcome::of(AttemptState::NotRouted);
            };
            match search.find_connection() {
                Ok(connection) => connection,
                Err(_) => return AttemptOutcome::of(AttemptState::NotRouted),
            }
        };
        // `:202-205` — a connection whose ends are on a disabled layer is
        // refused before anything is inserted.
        let start_layer = connection.start_layer().unwrap_or(0);
        let target_layer = connection.target_layer().unwrap_or(0);
        if !ctrl.layer_is_active(start_layer) || !ctrl.layer_is_active(target_layer) {
            return AttemptOutcome::of(AttemptState::NotRouted);
        }
        match crate::locate::locate(&engine, &connection) {
            // `:197-200` — a locator that refuses the chain is NOT_ROUTED.
            Err(_) => return AttemptOutcome::of(AttemptState::NotRouted),
            Ok(mut located) => {
                pin_ends_to_connection_points(engine.board, &mut located);
                (located, bounds)
            }
        }
    };

    let mut ripped: Vec<ItemId> = located.ripped_items.iter().map(|item| item.item).collect();
    ripped.sort_unstable();
    ripped.dedup();
    let mut outcome = AttemptOutcome {
        ripped,
        ..AttemptOutcome::default()
    };

    match crate::insert::insert_connection(board, rules, &ctrl, &located, bounds) {
        Ok(insert) => {
            // `BatchAutorouter.autoroute_item` (`:1327-1333`): every ROUTED
            // result is followed by a pull-tight over the region the insertion
            // touched.
            //
            // The three arguments upstream passes here are easy to get wrong:
            //
            // * `new int[0]` for the net filter — **all** nets in the region are
            //   optimized, not just the one just routed. Neighbours the new
            //   trace shoved aside get to relax too;
            // * `null` for the clip shape — the changed area is the only
            //   restriction;
            // * `TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP` (1000 ms) as the stall
            //   guard, which is armed here through
            //   [`crate::pull_tight::StallGuard`] so that the default (wall)
            //   mode is upstream's constant verbatim, per `ARCHITECTURE.md`
            //   "Budgets", and `--deterministic` spends the equivalent in
            //   optimization rounds.
            //
            // This used to be `None`, on the argument that a wall clock makes
            // the routed board depend on machine load (measured: `Issue034`
            // finished with 34 incomplete connections on an idle machine and 56
            // on a busy one). The argument was right about *why* the
            // deterministic mode exists and wrong about making it the default:
            // with no clock at all the only bound was a two-million round
            // counter, and §6.4 fork smoothing made a round cost a whole net's
            // `normalize_traces`. `Issue209-split10` then diverged instead of
            // converging and ran minutes past its `--job-timeout` holding a
            // board it never wrote. Both halves are back: upstream's constant
            // by default, the round budget under `--deterministic`, and the job
            // deadline polled underneath both.
            let mut changed_area = insert.changed_area;
            let mut algo = PullTightAlgo::new(
                board,
                rules,
                &[],
                None,
                ctrl.pull_tight_accuracy,
                None,
                None,
                0,
            )
            .with_stall_guard(StallGuard::for_mode(
                budget_mode,
                TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS,
            ));
            // Unconditional, including for an insertion that placed nothing: the
            // changed area also carries the region a *ripup* freed, and skipping
            // the relaxation of that region would move copper, not just a label.
            algo.opt_changed_area(&mut changed_area);

            // The ripup bookkeeping is done either way: a connection that tore
            // copper out and put none back still owes those nets a retry.
            outcome.changed_nets = insert.requeue_nets;
            outcome.ripped_connections = insert.ripped_connections;
            // **A connection that left no copper is not a route.** Upstream
            // reports `ROUTED` for one anyway — `InsertFoundConnectionAlgo`
            // accepts a located connection whose only item is a single corner
            // (`:95-103`), and `connect_to_trace` returns `true` for a corner
            // that already lies on the target trace (`RoutingBoard:1100-1105`),
            // on the assumption that the following `normalize_traces` will
            // split that trace at the point and make the contact. It cannot
            // when the trace is `USER_FIXED`: the split is refused
            // (`PolylineTrace.split:707-709`). `Issue229`, whose `(wiring)`
            // scope is entirely `(type protect)`, is that board end to end —
            // 39 connections, 39 `ROUTED`, and a session byte-identical in
            // copper to the input.
            //
            // **A divergence in the report only** (`docs/PARITY-DIVERGENCES.md`).
            // The board is bit-for-bit what upstream's rule would leave, because
            // the branch that changes is the *label*: an insertion that left no
            // item behind has by construction routed nothing.
            outcome.state = Some(AttemptState::Routed);
        }
        Err(_) => outcome.state = Some(AttemptState::InsertError),
    }
    outcome
}

/// Extends a located connection's two free ends to the **connection point** of
/// the drill item they land on.
///
/// `DrillItem.get_trace_connection_shape` is `TileShape.get_instance(center)`
/// — a *point* (`board/DrillItem.java:349-352`), so upstream's target shape for
/// a pin or a via is its centre and the located trace ends exactly there. The
/// M2a engine answers that query with the item's whole compensated pad instead
/// (`engine.rs`, `trace_connection_shape`, "pin-exit geometry lives with the
/// locate/insert wave"), so the locator stops on the pad boundary shrunk by the
/// half width — geometrically on the pad, and *topologically unconnected*: the
/// contact predicate is exact equality against the centre
/// (`drc-connectivity.md` §4.3), which is what `fr_board` and `fr_check` both
/// implement.
///
/// Prepending the centre restores upstream's endpoint without moving anything
/// the locator decided. It cannot introduce a violation: both the old end and
/// the centre lie in the pad shrunk by the trace half width, that region is
/// convex, so the added segment's whole body stays inside same-net pad copper.
///
/// Delete this the day `trace_connection_shape` answers a drill item with its
/// centre.
fn pin_ends_to_connection_points(board: &Board, located: &mut crate::locate::LocatedConnection) {
    let target_item = located.target_item;
    let start_item = Some(located.start_item);
    if let (Some(item), Some(first)) = (target_item, located.connection_items.first_mut()) {
        if let Some(center) = drill_center(board, item) {
            if let Some(first_corner) = first.corners.first() {
                let dx = (first_corner.x - center.x).abs();
                let dy = (first_corner.y - center.y).abs();
                if dx <= 10_000 && dy <= 10_000 && *first_corner != center {
                    first.corners.insert(0, center);
                }
            }
        }
    }
    if let (Some(item), Some(last)) = (start_item, located.connection_items.last_mut()) {
        if let Some(center) = drill_center(board, item) {
            if let Some(last_corner) = last.corners.last() {
                let dx = (last_corner.x - center.x).abs();
                let dy = (last_corner.y - center.y).abs();
                if dx <= 10_000 && dy <= 10_000 && *last_corner != center {
                    last.corners.push(center);
                }
            }
        }
    }
}

/// The centre of a pin or a via — its only connection point — or `None` for
/// every other class.
fn drill_center(board: &Board, id: ItemId) -> Option<fr_geometry::planar::IntPoint> {
    match &board.store().get(id)?.kind {
        ItemKind::Pin(pin) => Some(pin.center),
        ItemKind::Via(via) => Some(via.center),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// the pass
// ---------------------------------------------------------------------------

/// Everything one pass produced — `BatchAutorouter`'s `RouterCounters`, plus
/// the two flags the batch loop branches on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassOutcome {
    /// 1-based pass number, the ripup multiplier.
    pub pass_no: i32,
    /// Work-list length at pass start.
    pub queued: usize,
    /// `ROUTED` results.
    pub routed: usize,
    /// `ALREADY_CONNECTED` / `NO_UNCONNECTED_NETS` / `CONNECTED_TO_PLANE`.
    pub skipped: usize,
    /// `FAILED` / `INSERT_ERROR`.
    pub not_routed: usize,
    /// Total ripped items over the pass.
    pub ripped_item_count: usize,
    /// Connections requeued after a ripup (our divergence; see the module docs).
    pub requeued: usize,
    /// Ratsnest incompletes after the pass, tails already removed.
    pub incomplete_count: usize,
    /// The ripup budget the pass ran at: `start_ripup_costs * pass_no`.
    pub ripup_costs: i32,
    /// Milliseconds — wall, or virtual under
    /// [`BudgetMode::Deterministic`](budget::BudgetMode::Deterministic) — spent
    /// inside the pass's connections. Bench's `time_used`.
    pub time_used_ms: u64,
    /// Effort units charged over the pass's connections. Bench's `effort_used`.
    pub effort_used: u64,
    /// Connections that reached their [`budget`] allowance.
    pub connections_hitting_budget: usize,
    /// The pass stopped on `settings.maxItems` (`:496-505`). Upstream turns
    /// this into a stop request over every stage, so the batch loop ends here
    /// and the optimizer never runs.
    pub hit_item_cap: bool,
    /// `ROUTED` connections whose copper was gone by the end of the pass —
    /// deleted by the tail cleanup, or ripped out by a later connection —
    /// already moved out of [`routed`](PassOutcome::routed) and into
    /// [`not_routed`](PassOutcome::not_routed). A pass with a large number here
    /// is inserting copper that connects nothing; see
    /// `BatchRouter::discount_routes_that_left_no_copper`.
    pub routes_undone: usize,
    /// `autoroute_pass` returned false because the work list was empty — the
    /// board is complete (`:437-440`).
    pub board_complete: bool,
    /// `routed > 0 || not_routed > 0` (`:688`): something was attempted.
    pub progressed: bool,
}

/// The single-threaded batch autorouter.
///
/// Upstream runs strictly sequentially — the multi-thread pass is private and
/// has no call site (conformance #107, #111) — so this has no thread knob.
pub struct BatchRouter<'r> {
    rules: &'r BoardRules,
    settings: &'r RouterSettings,
    /// `BatchAutorouter.start_ripup_costs` (`:1279-1280`); pass `n` uses
    /// `start_ripup_costs * n`.
    pub start_ripup_costs: i32,
    /// `BatchAutorouter.remove_unconnected_vias`; drives the tail-cleanup stop
    /// option (`:657-661`).
    pub remove_unconnected_vias: bool,
    /// When a connection whose copper was ripped is re-attempted inside the
    /// same pass — the one declared divergence of this module. Default
    /// [`RequeuePolicy::SinglePassOnly`]: upstream's no-retry rule for
    /// `-mp >= 2`, the requeue for `-mp 1`.
    pub requeue_policy: RequeuePolicy,
    /// Whether the fanout stage ran, gating the batch loop's one-time
    /// fanout-tail recovery (`:1073`). Set by whoever ran the stage.
    pub fanout_enabled: bool,
    /// The per-connection budget policy — upstream's wall constants by default.
    pub budget: BudgetPolicy,
    /// `settings.maxItems` (`:496-505`), counted over the whole run.
    pub max_items: i32,
    /// Board units per millimetre, for the score's trace-length term. See
    /// [`CheckScorer::new`].
    pub board_units_per_mm: f64,
    /// The order a pass hands out its work list.
    ///
    /// [`portfolio::WorkOrder::Scan`] — the release's own descending-id order —
    /// unless this router is a [`portfolio`] arm. It is the *only* thing a
    /// portfolio arm is allowed to vary, so a router with the default order
    /// routes exactly what it routed before this field existed.
    pub work_order: portfolio::WorkOrder,
    /// How many workers may route connections concurrently in an autoroute pass.
    /// Default [`Threads::single`] (sequential).
    pub parallel_connections: threads::Threads,
    /// Whether the current run requeues; resolved from
    /// [`BatchRouter::requeue_policy`] by [`BatchRouter::run`].
    requeue_active: bool,
    total_items_routed: i32,
    scratch: ContactScratch,
    /// Copper the pass's `ROUTED` connections left on the board, one span per
    /// connection in `routed_copper_spans`. Kept on the router so a pass
    /// allocates nothing; see `discount_connections_the_cleanup_undid`.
    routed_copper: Vec<ItemId>,
    routed_copper_spans: Vec<(usize, usize)>,
    /// When set, [`autoroute_items`] is filtered to these nets. The optimizer's
    /// inner reroute uses this to skip leftover unrouted nets that have already
    /// been tried against the current board.
    pub net_filter: Option<std::collections::BTreeSet<i32>>,
}

impl<'r> BatchRouter<'r> {
    /// A router over these rules and settings, with the upstream defaults.
    ///
    /// `settings` must already have been through
    /// [`RouterSettings::apply_board_specific_optimizations`], or every layer
    /// costs 1.0/1.0.
    pub fn new(rules: &'r BoardRules, settings: &'r RouterSettings) -> BatchRouter<'r> {
        BatchRouter {
            rules,
            settings,
            start_ripup_costs: settings.scoring.start_ripup_costs(),
            remove_unconnected_vias: true,
            requeue_policy: RequeuePolicy::default(),
            fanout_enabled: false,
            budget: BudgetPolicy::default(),
            max_items: settings.max_items,
            board_units_per_mm: 1.0,
            parallel_connections: threads::Threads::single(),
            work_order: portfolio::WorkOrder::default(),
            // A direct `autoroute_pass` caller drives a single pass, which is
            // the case the requeue exists for; `run` overwrites this.
            requeue_active: true,
            total_items_routed: 0,
            scratch: ContactScratch::new(),
            routed_copper: Vec::new(),
            routed_copper_spans: Vec::new(),
            net_filter: None,
        }
    }

    /// Sets the parallel connection workers budget.
    pub fn with_parallel_connections(mut self, threads: threads::Threads) -> BatchRouter<'r> {
        self.parallel_connections = threads;
        self
    }

    /// Sets the trace-length unit conversion of the score
    /// ([`CheckScorer::new`]). The importer is the only thing that knows it.
    pub fn with_board_units_per_mm(mut self, board_units_per_mm: f64) -> BatchRouter<'r> {
        self.board_units_per_mm = board_units_per_mm;
        self
    }

    /// Records that the fanout stage ran, arming the batch loop's one-time
    /// fanout-tail recovery.
    pub fn with_fanout(mut self, ran: bool) -> BatchRouter<'r> {
        self.fanout_enabled = ran;
        self.remove_unconnected_vias = !ran;
        self
    }

    /// Sets the per-connection budget policy — `--deterministic` picks
    /// [`BudgetPolicy::deterministic`].
    pub fn with_budget(mut self, budget: BudgetPolicy) -> BatchRouter<'r> {
        self.budget = budget;
        self
    }

    /// Sets the pass work-list order. Only [`portfolio::race`] passes anything
    /// but the default.
    pub fn with_work_order(mut self, work_order: portfolio::WorkOrder) -> BatchRouter<'r> {
        self.work_order = work_order;
        self
    }

    /// Items routed so far, across every pass — upstream's `totalItemsRouted`.
    pub fn total_items_routed(&self) -> i32 {
        self.total_items_routed
    }

    /// Whether the pass currently being run re-attempts ripped connections.
    pub fn requeue_active(&self) -> bool {
        self.requeue_active
    }

    /// Resolves the requeue for a caller that drives
    /// [`autoroute_pass`](BatchRouter::autoroute_pass) itself.
    ///
    /// [`run`](BatchRouter::run) resolves [`requeue_policy`](BatchRouter::requeue_policy)
    /// against the pass cap, because [`RequeuePolicy::SinglePassOnly`] is a
    /// statement about the *run*. A direct-drive caller — the optimizer's
    /// [`MazeRerouter`](crate::optimizer::MazeRerouter), which loops
    /// `autoroute_pass` exactly as
    /// `BatchAutorouter.autoroute_passes_for_optimizing_item` (`:156-161`) does
    /// — never goes through `run` and so has to say which behaviour it wants.
    /// Upstream re-derives the work list on the next of its passes instead of
    /// requeueing inside one, so that caller wants `false`.
    pub fn set_requeue_active(&mut self, active: bool) {
        self.requeue_active = active;
    }

    /// One pass — `BatchAutorouter.autoroute_pass` (`:431-694`).
    ///
    /// `pass_no` is 1-based and multiplies the ripup budget. Runs under the
    /// default budget policy for that pass; the batch loop calls
    /// [`BatchRouter::autoroute_pass_with`] with the context it built.
    pub fn autoroute_pass(&mut self, board: &mut Board, pass_no: i32) -> PassOutcome {
        let ctx = PassContext::new(
            pass_no,
            self.start_ripup_costs,
            &self.budget,
            self.remove_unconnected_vias,
        );
        self.autoroute_pass_with(board, &ctx)
    }

    /// Moves every `ROUTED` connection whose copper is no longer on the board
    /// when the pass ends into `not_routed`.
    ///
    /// **A declared divergence, in the report only**
    /// (`docs/PARITY-DIVERGENCES.md`). Upstream counts a connection as routed
    /// the moment the insertion returns and never looks again — so a pass that
    /// inserts a stub, finds it dangling and deletes it in the end-of-pass
    /// cleanup two lines later still reports it as a route, and so does a pass
    /// whose copper a later connection ripped straight back out. Neither
    /// connection is on the board, and the second is exactly the one the
    /// requeue is about to attempt again, so counting it here counts it twice.
    ///
    /// `Issue229` is the extreme case. Its `(wiring)` scope is entirely
    /// `(type protect)`, no `USER_FIXED` trace can be split at a junction
    /// (`PolylineTrace.split:707-709`), so nothing the router inserts can ever
    /// make a contact: every insertion is a tail, `remove_tails` deletes all of
    /// it, and the pre-fix binary reported **648 routes** over 18 passes for a
    /// session whose copper is byte-identical to the input's.
    ///
    /// The board is untouched by any of this — only the label changes.
    fn discount_routes_that_left_no_copper(&self, board: &Board, outcome: &mut PassOutcome) {
        let undone = self
            .routed_copper_spans
            .iter()
            .filter(|(start, end)| {
                !self.routed_copper[*start..*end]
                    .iter()
                    .any(|id| board.store().contains(*id))
            })
            .count();
        outcome.routed = outcome.routed.saturating_sub(undone);
        outcome.not_routed += undone;
        outcome.routes_undone = undone;
    }

    /// [`BatchRouter::autoroute_pass`] with the schedule's own context.
    pub fn autoroute_pass_with(&mut self, board: &mut Board, ctx: &PassContext) -> PassOutcome {
        let mut outcome = PassOutcome {
            pass_no: ctx.pass_no,
            ripup_costs: ctx.ripup_costs,
            ..PassOutcome::default()
        };
        // Reused across passes, so a run allocates these once.
        self.routed_copper.clear();
        self.routed_copper_spans.clear();
        let mut work = autoroute_items(board, self.rules, &mut self.scratch);
        if let Some(filter) = &self.net_filter {
            work.retain(|item| filter.contains(&item.net_no));
        }
        // A no-op under the default order; see `portfolio::WorkOrder`.
        self.work_order.apply(&mut work, ctx.pass_no);
        outcome.queued = work.len();
        if work.is_empty() {
            // `:437-440` — the board is complete.
            outcome.board_complete = true;
            outcome.incomplete_count = incomplete_count(board);
            return outcome;
        }

        let mut queue = work;
        // Requeue budget: every entry may be retried once per ripup that
        // touched it, and the pass can never grow past twice its work list.
        let requeue_budget = queue.len();

        if self.parallel_connections.is_parallel() {
            self.autoroute_pass_parallel(board, ctx, &mut outcome, &mut queue, requeue_budget);
        } else {
            let mut index = 0;
            while index < queue.len() {
                let entry = queue[index];
                index += 1;
                // `:496-505` — the item cap. Upstream calls `requestStop()` (ALL
                // stages) rather than `request_stop_auto_router()`, so hitting it
                // also skips the optimizer; the flag is what carries that to the
                // batch loop and to `optimizer::route_then_optimize`.
                if self.max_items > 0 && self.total_items_routed >= self.max_items {
                    outcome.hit_item_cap = true;
                    break;
                }
                // The job's wall budget, polled **between connections** — the only
                // place inside a pass where the board is in a consistent state and
                // nothing is half-inserted. `schedule::BatchLoop` reads the same
                // deadline after the pass returns and ends the run there.
                if ctx.deadline.expired() {
                    break;
                }
                self.route_single_serial(
                    board,
                    ctx,
                    &mut outcome,
                    &mut queue,
                    requeue_budget,
                    entry,
                );
            }
        }

        // `:657-661` — end-of-pass tail cleanup.
        let stop = if ctx.remove_unconnected_vias {
            StopConnection::None
        } else {
            StopConnection::FanoutVia
        };
        remove_tails(board, -1, stop, &mut self.scratch);
        self.discount_routes_that_left_no_copper(board, &mut outcome);

        outcome.incomplete_count = incomplete_count(board);
        outcome.progressed = outcome.routed > 0 || outcome.not_routed > 0;
        outcome
    }

    fn route_single_serial(
        &mut self,
        board: &mut Board,
        ctx: &PassContext,
        outcome: &mut PassOutcome,
        queue: &mut Vec<WorkItem>,
        requeue_budget: usize,
        entry: WorkItem,
    ) {
        self.total_items_routed += 1;
        let first_new_id = board.store().next_id();
        let expansions_before = crate::maze::trace::is_enabled()
            .then(|| fr_metrics::counters::value(fr_metrics::counters::Counter::Expansions));
        let incomplete_before =
            crate::optimizer::trace::is_enabled().then(|| incomplete_count(board));
        let mut clock = ctx.connection_budget().start();
        let attempt = autoroute_item(
            board,
            self.rules,
            self.settings,
            entry.item,
            entry.net_no,
            ctx.ripup_costs,
            ctx.remove_unconnected_vias,
            Some(&mut clock),
            &mut self.scratch,
        );
        let usage = clock.finish();
        if let Some(expansions_before) = expansions_before {
            emit_conn_landing(
                board,
                ctx.pass_no,
                entry,
                first_new_id,
                expansions_before,
                &attempt,
            );
        }
        if let Some(incomplete_before) = incomplete_before {
            crate::optimizer::trace::Conn {
                pass_no: ctx.pass_no,
                net_no: entry.net_no,
                state: attempt.state(),
                ripped: attempt.ripped.len(),
                budget_ms: ctx.connection_budget().limit_ms(),
                used_ms: usage.time_used_ms,
                hit_budget: usage.hit_budget,
                incomplete: (incomplete_before, incomplete_count(board)),
            }
            .emit();
        }
        outcome.time_used_ms += usage.time_used_ms;
        outcome.effort_used += usage.effort_used;
        if usage.hit_budget {
            outcome.connections_hitting_budget += 1;
        }
        match attempt.state() {
            AttemptState::Routed => {
                outcome.routed += 1;
                let start = self.routed_copper.len();
                self.routed_copper.extend(
                    board
                        .store()
                        .iter()
                        .map(|(id, _)| id)
                        .take_while(|id| *id >= first_new_id),
                );
                self.routed_copper_spans
                    .push((start, self.routed_copper.len()));
            }
            state if state.is_skip() => outcome.skipped += 1,
            _ => outcome.not_routed += 1,
        }
        outcome.ripped_item_count += attempt.ripped.len();
        if self.requeue_active && outcome.requeued < requeue_budget {
            for net_no in attempt.changed_nets.iter().copied() {
                if net_no == entry.net_no {
                    continue;
                }
                if let Some(seed) = requeue_seed(board, net_no) {
                    queue.push(WorkItem { item: seed, net_no });
                    outcome.requeued += 1;
                }
            }
        }
    }

    fn autoroute_pass_parallel(
        &mut self,
        board: &mut Board,
        ctx: &PassContext,
        outcome: &mut PassOutcome,
        queue: &mut Vec<WorkItem>,
        requeue_budget: usize,
    ) {
        let thread_count = self.parallel_connections.get();
        let max_batch_size = (thread_count * 2).max(2);
        let mut current_batch_size = max_batch_size;
        let budget_mode = ctx.connection_budget().mode();

        let mut index = 0;
        let mut consecutive_serial = 0;
        while index < queue.len() {
            if self.max_items > 0 && self.total_items_routed >= self.max_items {
                outcome.hit_item_cap = true;
                break;
            }
            if ctx.deadline.expired() {
                break;
            }

            let chunk_len = (queue.len() - index).min(current_batch_size);
            if chunk_len <= 1 {
                let entry = queue[index];
                index += 1;
                self.route_single_serial(board, ctx, outcome, queue, requeue_budget, entry);
                consecutive_serial += 1;
                if consecutive_serial >= 8 && current_batch_size < max_batch_size {
                    current_batch_size = (current_batch_size * 2).min(max_batch_size);
                    consecutive_serial = 0;
                }
                continue;
            }
            consecutive_serial = 0;

            let chunk_items = &queue[index..index + chunk_len];
            let actual_workers = self.parallel_connections.capped_to(chunk_len).get();

            let cursor = std::sync::atomic::AtomicUsize::new(0);
            let candidates: Vec<parallel_conn::CandidateAttempt> = std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(actual_workers);
                for _ in 0..actual_workers {
                    let mut worker_board = board.clone();
                    let rules = self.rules;
                    let settings = self.settings;
                    let budget_policy = ctx.connection_budget();
                    let ripup_costs = ctx.ripup_costs;
                    let remove_vias = ctx.remove_unconnected_vias;
                    let cursor_ref = &cursor;

                    handles.push(s.spawn(move || {
                        let mut scratch = ContactScratch::new();
                        let mut results = Vec::new();
                        while let Ok(i) = cursor_ref.fetch_update(
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                            |curr| {
                                if curr < chunk_len {
                                    Some(curr + 1)
                                } else {
                                    None
                                }
                            },
                        ) {
                            let item = chunk_items[i];
                            let attempt = parallel_conn::evaluate_candidate(
                                &mut worker_board,
                                rules,
                                settings,
                                item,
                                ripup_costs,
                                remove_vias,
                                budget_policy,
                                &mut scratch,
                            );
                            results.push((i, attempt));
                        }
                        results
                    }));
                }

                let mut collected = Vec::with_capacity(chunk_len);
                for handle in handles {
                    if let Ok(results) = handle.join() {
                        collected.extend(results);
                    }
                }
                collected.sort_by_key(|(i, _)| *i);
                collected.into_iter().map(|(_, attempt)| attempt).collect()
            });

            index += chunk_len;

            let mut committed_write_boxes: Vec<fr_geometry::planar::IntBox> = Vec::new();
            let mut committed_items: BTreeSet<ItemId> = BTreeSet::new();
            let mut batch_conflicts = 0;

            for c in candidates {
                if self.max_items > 0 && self.total_items_routed >= self.max_items {
                    outcome.hit_item_cap = true;
                    break;
                }
                if ctx.deadline.expired() {
                    break;
                }

                let entry = c.work_item;
                let mut conflict = false;

                for wbox in &committed_write_boxes {
                    if c.read_boxes.iter().any(|b| b.intersects(wbox))
                        || c.write_boxes.iter().any(|b| b.intersects(wbox))
                    {
                        conflict = true;
                        break;
                    }
                }

                if !conflict {
                    for id in &c.touched_items {
                        if committed_items.contains(id) {
                            conflict = true;
                            break;
                        }
                    }
                }

                if !conflict {
                    for id in &c.outcome.ripped {
                        if committed_items.contains(id) {
                            conflict = true;
                            break;
                        }
                    }
                }

                if !conflict {
                    match c.state {
                        AttemptState::Routed => {
                            let first_new_id = board.store().next_id();
                            let mark = board.begin();
                            let mut committed = false;

                            if let (Some(located), Some(ctrl)) = (&c.located, &c.ctrl) {
                                if let Ok(insert) = crate::insert::insert_connection(
                                    board, self.rules, ctrl, located, c.bounds,
                                ) {
                                    let mut changed_area = insert.changed_area;
                                    let mut algo = PullTightAlgo::new(
                                        board,
                                        self.rules,
                                        &[],
                                        None,
                                        ctrl.pull_tight_accuracy,
                                        None,
                                        None,
                                        0,
                                    )
                                    .with_stall_guard(StallGuard::for_mode(
                                        budget_mode,
                                        TIME_LIMIT_TO_PREVENT_ENDLESS_LOOP_MS,
                                    ));
                                    algo.opt_changed_area(&mut changed_area);

                                    if insert.board_changed {
                                        board.commit(mark);
                                        committed = true;
                                        committed_write_boxes.extend(c.write_boxes);
                                        for id in &c.touched_items {
                                            committed_items.insert(*id);
                                        }
                                        for id in &c.outcome.ripped {
                                            committed_items.insert(*id);
                                        }

                                        self.total_items_routed += 1;
                                        outcome.time_used_ms += c.usage.time_used_ms;
                                        outcome.effort_used += c.usage.effort_used;
                                        if c.usage.hit_budget {
                                            outcome.connections_hitting_budget += 1;
                                        }
                                        outcome.routed += 1;
                                        let start = self.routed_copper.len();
                                        self.routed_copper.extend(
                                            board
                                                .store()
                                                .iter()
                                                .map(|(id, _)| id)
                                                .take_while(|id| *id >= first_new_id),
                                        );
                                        self.routed_copper_spans
                                            .push((start, self.routed_copper.len()));
                                        outcome.ripped_item_count += c.outcome.ripped.len();

                                        if self.requeue_active && outcome.requeued < requeue_budget
                                        {
                                            for net_no in c.outcome.changed_nets.iter().copied() {
                                                if net_no != entry.net_no {
                                                    if let Some(seed) = requeue_seed(board, net_no)
                                                    {
                                                        queue.push(WorkItem { item: seed, net_no });
                                                        outcome.requeued += 1;
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        board.rollback_to(mark);
                                    }
                                } else {
                                    board.rollback_to(mark);
                                }
                            }

                            if !committed {
                                batch_conflicts += 1;
                                self.route_single_serial(
                                    board,
                                    ctx,
                                    outcome,
                                    queue,
                                    requeue_budget,
                                    entry,
                                );
                            }
                        }
                        state if state.is_skip() => {
                            self.total_items_routed += 1;
                            outcome.skipped += 1;
                        }
                        _ => {
                            self.total_items_routed += 1;
                            outcome.not_routed += 1;
                            outcome.time_used_ms += c.usage.time_used_ms;
                            outcome.effort_used += c.usage.effort_used;
                            if c.usage.hit_budget {
                                outcome.connections_hitting_budget += 1;
                            }
                        }
                    }
                } else {
                    batch_conflicts += 1;
                    if c.state == AttemptState::Routed {
                        let first_new_id = board.store().next_id();
                        self.route_single_serial(board, ctx, outcome, queue, requeue_budget, entry);
                        let cl = (0..self.rules.layer_structure.len())
                            .map(|l| self.rules.clearance_matrix.max_value_on_layer(l))
                            .max()
                            .unwrap_or(0) as f64;
                        for (id, item) in board.store().iter() {
                            if id < first_new_id {
                                break;
                            }
                            match &item.kind {
                                ItemKind::Trace(tr) => {
                                    let hw = tr.half_width as f64;
                                    for w in tr.polyline.corner_arr().windows(2) {
                                        let p1 = match &w[0] {
                                            Point::Int(ip) => *ip,
                                            other => other.to_float().round(),
                                        };
                                        let p2 = match &w[1] {
                                            Point::Int(ip) => *ip,
                                            other => other.to_float().round(),
                                        };
                                        committed_write_boxes.push(
                                            fr_geometry::planar::IntBox::new_from_points(p1, p2)
                                                .offset(cl + hw),
                                        );
                                    }
                                }
                                ItemKind::Via(via) => {
                                    committed_write_boxes.push(
                                        fr_geometry::planar::IntBox::new(
                                            via.center.x - 500,
                                            via.center.y - 500,
                                            via.center.x + 500,
                                            via.center.y + 500,
                                        )
                                        .offset(cl),
                                    );
                                }
                                _ => {}
                            }
                        }
                    } else {
                        self.total_items_routed += 1;
                        outcome.not_routed += 1;
                        outcome.time_used_ms += c.usage.time_used_ms;
                        outcome.effort_used += c.usage.effort_used;
                        if c.usage.hit_budget {
                            outcome.connections_hitting_budget += 1;
                        }
                    }
                }
            }

            if batch_conflicts * 2 >= chunk_len {
                current_batch_size = (current_batch_size / 2).max(1);
            } else if batch_conflicts == 0 {
                current_batch_size = (current_batch_size * 2).min(max_batch_size);
            }
        }
    }

    /// The full batch loop — `BatchAutorouter.runBatchLoop` (`:757-1188`), i.e.
    /// [`schedule::BatchLoop`] with this router as its driver and
    /// [`CheckScorer`] as its scorer.
    ///
    /// `max_passes` is clamped to at least 1: upstream's `maxPasses <= 0`
    /// escape hatch runs the loop **uncapped** (conformance appendix §1.2), and
    /// this port refuses to produce that state from a pass count — the CLI
    /// rejects a negative `-mp` outright, and [`schedule::LoopConfig`] is the
    /// only way to ask for the uncapped loop deliberately.
    pub fn run(&mut self, board: &mut Board, max_passes: i32) -> RunOutcome {
        let max_passes = max_passes.max(1);
        self.requeue_active = self.requeue_policy.applies_to(max_passes);
        let config = LoopConfig {
            max_passes,
            start_ripup_costs: self.start_ripup_costs,
            fanout_enabled: self.fanout_enabled,
            remove_unconnected_vias: self.remove_unconnected_vias,
            history_limit: history::MAX_HISTORY_SIZE,
            budget: self.budget,
            deadline: schedule::JobDeadline::ambient(),
        };
        // Copied out of `self` so the scorer's borrow of the rules is
        // independent of the `&mut self` the loop takes as its driver.
        let rules: &'r BoardRules = self.rules;
        let mut scorer = CheckScorer::new(rules, self.board_units_per_mm)
            .with_weights(scoring::weights_from(&self.settings.scoring));
        BatchLoop::new(config).run(board, self, &mut scorer)
    }
}

impl PassDriver for BatchRouter<'_> {
    fn run_pass(&mut self, board: &mut Board, ctx: &PassContext) -> PassOutcome {
        self.autoroute_pass_with(board, ctx)
    }

    fn remove_tails(&mut self, board: &mut Board, stop: StopConnection) {
        remove_tails(board, -1, stop, &mut self.scratch);
    }

    fn on_board_restored(&mut self, _board: &Board, _delta: &StoreDelta) {
        // The restored board carries its own search trees and connectivity, so
        // nothing *inside* it is stale, and this router builds its engine — and
        // therefore its room database — per connection, so there is no room
        // cache to clear either. Dropping the scratch buffers is the whole of
        // the router-side invalidation today; this is the seam a persistent
        // room database subscribes to, with `delta` telling it which items
        // moved (`ARCHITECTURE.md`, fr-board mechanism 2).
        self.scratch = ContactScratch::new();
    }
}

/// The seed item a requeued net is re-attempted from: the lowest-id connectable
/// non-routable member, i.e. the same class of item
/// [`autoroute_items`] enqueues.
fn requeue_seed(board: &Board, net_no: i32) -> Option<ItemId> {
    board.store().net_items(net_no).iter().copied().find(|id| {
        board
            .store()
            .get(*id)
            .is_some_and(|item| item.is_connectable() && !item.is_routable())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestFixture, TEST_HALF_WIDTH, TEST_NET};
    use fr_board::net_nos::NetNos;
    use fr_geometry::planar::IntPoint;

    #[test]
    fn the_work_list_holds_one_entry_per_unconnected_seed() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut scratch = ContactScratch::new();
        let work = autoroute_items(&fixture.board, &fixture.rules, &mut scratch);
        // Two pads, one net, nothing joining them: both are seeds, because
        // each one's connected set is smaller than the net.
        assert_eq!(work.len(), 2);
        assert!(work.iter().all(|entry| entry.net_no == TEST_NET));
    }

    #[test]
    fn the_work_list_runs_in_descending_id_order() {
        let fixture = TestFixture::two_layer_with_pads();
        let mut scratch = ContactScratch::new();
        let work = autoroute_items(&fixture.board, &fixture.rules, &mut scratch);
        let ids: Vec<ItemId> = work.iter().map(|entry| entry.item).collect();
        let mut descending = ids.clone();
        descending.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(ids, descending, "the scan is ItemStore::iter, descending");
    }

    #[test]
    fn a_connected_net_is_not_queued() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let centers = fixture.pad_centers();
        fixture
            .board
            .insert_trace_points(
                &[centers[0], centers[1]],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the straight wire inserts");
        let mut scratch = ContactScratch::new();
        let work = autoroute_items(&fixture.board, &fixture.rules, &mut scratch);
        assert!(work.is_empty(), "a joined net has no work: {work:?}");
    }

    #[test]
    fn the_incomplete_count_is_components_minus_one() {
        let mut fixture = TestFixture::two_layer_with_pads();
        assert_eq!(incomplete_count(&mut fixture.board), 1);
        let centers = fixture.pad_centers();
        fixture
            .board
            .insert_trace_points(
                &[centers[0], centers[1]],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the straight wire inserts");
        assert_eq!(incomplete_count(&mut fixture.board), 0);
    }

    #[test]
    fn one_pass_routes_the_two_pad_board() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let mut router = BatchRouter::new(&rules, &settings);
        let outcome = router.run(&mut fixture.board, 1);
        assert_eq!(outcome.incomplete_count, 0, "{outcome:?}");
        assert!(outcome.passes[0].routed >= 1);
    }

    #[test]
    fn one_connection_goes_through_the_engine_locator_and_inserter() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let pads = fixture.pad_ids();
        let ctrl = fixture.control();
        let rules = fixture.rules.clone();
        let outcome = route_with_control(
            &mut fixture.board,
            &rules,
            ctrl,
            &pads[..1],
            &pads[1..2],
            None,
        );
        assert_eq!(outcome.state(), AttemptState::Routed, "{outcome:?}");
        assert!(outcome.ripped.is_empty(), "an empty board rips nothing");
        assert_eq!(incomplete_count(&mut fixture.board), 0);
    }

    /// A pass over a board with a foreign-net trace across the middle either
    /// routes around it or rips it; either way the counters add up and the
    /// ripped items are reported. `routed + skipped + not_routed` must equal
    /// what the pass attempted (`BatchAutorouter.autoroute_pass:607-631`).
    #[test]
    fn the_pass_counters_account_for_every_attempt() {
        let mut fixture = TestFixture::with_foreign_trace();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();
        let mut router = BatchRouter::new(&rules, &settings);
        let pass = router.autoroute_pass(&mut fixture.board, 1);

        assert!(!pass.board_complete, "the net starts unrouted");
        assert_eq!(
            pass.routed + pass.skipped + pass.not_routed,
            router.total_items_routed() as usize,
            "{pass:?}"
        );
        assert_eq!(pass.progressed, pass.routed > 0 || pass.not_routed > 0);
        // The ripup budget of pass 1 is `start_ripup_costs * 1`, the effective
        // upstream default (conformance D5), not the ctor's dead 1000.
        assert_eq!(router.start_ripup_costs, START_RIPUP_COSTS);
        // Requeues never exceed the pass's own work-list length.
        assert!(pass.requeued <= pass.queued, "{pass:?}");
    }

    /// The whole budget chain, end to end: the pass arms the clock, the engine
    /// carries it, the maze charges one unit per settled element and bails when
    /// the allowance is gone, and the pass counts the connection as having hit
    /// its budget rather than as a routing failure it cannot explain.
    #[test]
    fn a_binding_budget_stops_every_connection_and_the_pass_accounts_for_it() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let rules = fixture.rules.clone();
        let settings = fixture.settings.clone();

        // The same board routes under the default (never-binding) policy ...
        let mut free = BatchRouter::new(&rules, &settings);
        let free_pass = free.autoroute_pass(&mut fixture.board.clone(), 1);
        assert!(free_pass.routed >= 1, "{free_pass:?}");
        assert_eq!(free_pass.connections_hitting_budget, 0, "{free_pass:?}");
        assert!(
            free_pass.effort_used > 0,
            "settles are charged: {free_pass:?}"
        );

        // ... and does not once one settled element is all a connection gets.
        let mut router = BatchRouter::new(&rules, &settings)
            .with_budget(BudgetPolicy::deterministic().with_effort_ceiling(Some(1)));
        let pass = router.autoroute_pass(&mut fixture.board, 1);

        assert_eq!(pass.routed, 0, "no connection can finish: {pass:?}");
        assert_eq!(pass.not_routed, pass.queued, "{pass:?}");
        assert_eq!(
            pass.connections_hitting_budget, pass.queued,
            "every connection reached its allowance: {pass:?}"
        );
        assert_eq!(pass.effort_used, pass.queued as u64, "one settle each");
        assert_eq!(
            incomplete_count(&mut fixture.board),
            1,
            "nothing was routed"
        );
    }

    #[test]
    fn removing_tails_deletes_a_dangling_wire() {
        let mut fixture = TestFixture::two_layer_with_pads();
        let centers = fixture.pad_centers();
        let dangling = fixture
            .board
            .insert_trace_points(
                &[centers[0], IntPoint::new(centers[0].x + 500, centers[0].y)],
                0,
                TEST_HALF_WIDTH,
                NetNos::of(TEST_NET),
                1,
                FixedState::Unfixed,
                None,
            )
            .expect("the stub inserts");
        let mut scratch = ContactScratch::new();
        assert!(remove_tails(
            &mut fixture.board,
            TEST_NET,
            StopConnection::None,
            &mut scratch
        ));
        assert!(fixture.board.store().get(dangling).is_none());
    }

    #[test]
    fn conduction_area_normal_contacts_and_connected_set_traversal() {
        use fr_board::item::{AreaShape, ConductionArea, LayerRange, PlacedArea, Via};
        use fr_geometry::planar::IntBox;

        let mut fixture = TestFixture::two_layer_with_pads();
        // Insert a conduction area on layer 1 covering the whole board
        let pour_id = fixture.board.insert_item(Item::new(
            NetNos::of(TEST_NET),
            1,
            FixedState::SystemFixed,
            ItemKind::ConductionArea(ConductionArea {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(-10000, -10000, 10000, 10000))),
                layer: 1,
                is_obstacle: false,
                is_filled: true,
            }),
        ));

        // Insert via 1 on net 1 spanning [0, 1] at (100, 100)
        let via1 = fixture.board.insert_item(Item::new(
            NetNos::of(TEST_NET),
            1,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(100, 100),
                padstack_no: 0,
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        ));

        // Insert via 2 on net 1 spanning [0, 1] at (500, 500)
        let via2 = fixture.board.insert_item(Item::new(
            NetNos::of(TEST_NET),
            1,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(500, 500),
                padstack_no: 0,
                layers: LayerRange::new(0, 1),
                attach_allowed: true,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        ));

        let mut scratch = ContactScratch::new();
        let mut pour_contacts = Vec::new();
        normal_contacts(&fixture.board, pour_id, &mut scratch, &mut pour_contacts);
        assert!(pour_contacts.contains(&via1), "pour must touch via1");
        assert!(pour_contacts.contains(&via2), "pour must touch via2");

        // Connected set starting at via1 must reach via2 through the pour
        let connected = connected_set(&fixture.board, TEST_NET, via1, &mut scratch);
        assert!(connected.contains(&via1));
        assert!(connected.contains(&pour_id));
        assert!(
            connected.contains(&via2),
            "connected_set must traverse through conduction area"
        );
    }
}
