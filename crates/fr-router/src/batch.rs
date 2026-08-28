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
//!
//! Corner placement and insertion are **not** here: this module hands the found
//! connection to [`crate::locate`] and [`crate::insert`], the port of
//! `LocateFoundConnectionAlgo` / `InsertFoundConnectionAlgo`.
//!
//! # M2a scope
//!
//! * **One pass is the contract.** [`BatchRouter::run`] repeats
//!   [`BatchRouter::autoroute_pass`] up to `max_passes` times with the upstream
//!   ripup escalation, and stops on "nothing left" or "no progress". The real
//!   schedule — `BoardHistory` snapshots, the pass ≥ 8 restore check, the two
//!   stagnation trackers (`batch-orchestration.md` §5.2) — is M2c. Every hook it
//!   needs is a field of [`PassOutcome`], so it slots in without moving code.
//! * **Ripped connections are requeued inside the pass.** Upstream re-derives
//!   the work list next pass instead; with `max_passes = 1` there is no next
//!   pass. Declared in `docs/PARITY-DIVERGENCES.md`.
//! * The per-connection pull-tight (`opt_changed_area`, `:1327-1333`) is M2b;
//!   [`crate::insert::InsertOutcome::changed_area`] already carries its input.
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
use fr_geometry::planar::Point;
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::ctrl::AutorouteControl;
use crate::engine::AutorouteEngine;
use crate::maze::search::MazeSearch;

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

/// Routes one net of one item — `BatchAutorouter.autoroute_item` (`:1256-1366`)
/// plus `AutorouteEngine.autoroute_connection` (`:127-260`), which the release
/// splits only because the engine owns the room database.
///
/// `ripup_costs` is the pass's `start_ripup_costs * pass_no`; the caller owns
/// the escalation (conformance D5 — `AutorouteControl`'s own default is dead).
#[allow(clippy::too_many_arguments)]
pub fn autoroute_item(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    item: ItemId,
    net_no: i32,
    ripup_costs: i32,
    remove_unconnected_vias: bool,
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

    let via_costs = if contains_plane {
        settings.scoring.plane_via_costs()
    } else {
        settings.scoring.via_costs()
    };
    let mut ctrl = AutorouteControl::for_net_with_costs(board, rules, settings, net_no, via_costs);
    ctrl.ripup_allowed = true;
    ctrl.ripup_costs = ripup_costs;
    ctrl.remove_unconnected_vias = remove_unconnected_vias;

    route_with_control(board, rules, ctrl, &start, &destination)
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
pub fn route_with_control(
    board: &mut Board,
    rules: &BoardRules,
    ctrl: AutorouteControl,
    start: &[ItemId],
    destination: &[ItemId],
) -> AttemptOutcome {
    board.ensure_compensated_tree(ctrl.trace_clearance_class_no);
    let (located, bounds) = {
        let mut engine = AutorouteEngine::new(board, rules, ctrl.clone());
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
            // M2b: `board.opt_changed_area(insert.changed_area, …)` — the
            // pull-tight pass upstream runs on every ROUTED result (`:1327-1333`).
            outcome.changed_nets = insert.requeue_nets;
            outcome.ripped_connections = insert.ripped_connections;
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
            if first.corners.first() != Some(&center) {
                first.corners.insert(0, center);
            }
        }
    }
    if let (Some(item), Some(last)) = (start_item, located.connection_items.last_mut()) {
        if let Some(center) = drill_center(board, item) {
            if last.corners.last() != Some(&center) {
                last.corners.push(center);
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
    /// `autoroute_pass` returned false because the work list was empty — the
    /// board is complete (`:437-440`).
    pub board_complete: bool,
    /// `routed > 0 || not_routed > 0` (`:688`): something was attempted.
    pub progressed: bool,
}

/// Everything a [`BatchRouter::run`] produced.
#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    /// One entry per pass actually run.
    pub passes: Vec<PassOutcome>,
    /// Ratsnest incompletes when the run stopped.
    pub incomplete_count: usize,
    /// Items inserted over the whole run.
    pub inserted_count: usize,
}

impl RunOutcome {
    /// Nothing left unconnected.
    pub fn is_complete(&self) -> bool {
        self.incomplete_count == 0
    }
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
    /// Re-attempt a connection whose wiring a later connection ripped, inside
    /// the same pass. **Divergence**, on by default: upstream relies on the
    /// next pass re-deriving the work list, which a one-pass run never gets.
    pub requeue_ripped_items: bool,
    /// `settings.maxItems` (`:496-505`), counted over the whole run.
    pub max_items: i32,
    total_items_routed: i32,
    scratch: ContactScratch,
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
            remove_unconnected_vias: false,
            requeue_ripped_items: true,
            max_items: settings.max_items,
            total_items_routed: 0,
            scratch: ContactScratch::new(),
        }
    }

    /// Items routed so far, across every pass — upstream's `totalItemsRouted`.
    pub fn total_items_routed(&self) -> i32 {
        self.total_items_routed
    }

    /// One pass — `BatchAutorouter.autoroute_pass` (`:431-694`).
    ///
    /// `pass_no` is 1-based and multiplies the ripup budget.
    pub fn autoroute_pass(&mut self, board: &mut Board, pass_no: i32) -> PassOutcome {
        let mut outcome = PassOutcome {
            pass_no,
            ..PassOutcome::default()
        };
        let work = autoroute_items(board, self.rules, &mut self.scratch);
        outcome.queued = work.len();
        if work.is_empty() {
            // `:437-440` — the board is complete.
            outcome.board_complete = true;
            outcome.incomplete_count = incomplete_count(board);
            return outcome;
        }
        let ripup_costs = self.start_ripup_costs.saturating_mul(pass_no);

        let mut queue = work;
        let mut index = 0;
        // Requeue budget: every entry may be retried once per ripup that
        // touched it, and the pass can never grow past twice its work list.
        let requeue_budget = queue.len();
        while index < queue.len() {
            let entry = queue[index];
            index += 1;
            if self.max_items > 0 && self.total_items_routed >= self.max_items {
                break;
            }
            self.total_items_routed += 1;
            let attempt = autoroute_item(
                board,
                self.rules,
                self.settings,
                entry.item,
                entry.net_no,
                ripup_costs,
                self.remove_unconnected_vias,
                &mut self.scratch,
            );
            match attempt.state() {
                AttemptState::Routed => outcome.routed += 1,
                state if state.is_skip() => outcome.skipped += 1,
                _ => outcome.not_routed += 1,
            }
            outcome.ripped_item_count += attempt.ripped.len();
            if self.requeue_ripped_items && outcome.requeued < requeue_budget {
                for net_no in attempt.changed_nets.iter().copied() {
                    if net_no == entry.net_no {
                        continue;
                    }
                    // The ripped net lost wiring; give it another attempt in
                    // this pass rather than waiting for a pass that may not
                    // come.
                    if let Some(seed) = requeue_seed(board, net_no) {
                        queue.push(WorkItem { item: seed, net_no });
                        outcome.requeued += 1;
                    }
                }
            }
        }

        // `:657-661` — end-of-pass tail cleanup.
        let stop = if self.remove_unconnected_vias {
            StopConnection::None
        } else {
            StopConnection::FanoutVia
        };
        remove_tails(board, -1, stop, &mut self.scratch);

        outcome.incomplete_count = incomplete_count(board);
        outcome.progressed = outcome.routed > 0 || outcome.not_routed > 0;
        outcome
    }

    /// Repeats [`BatchRouter::autoroute_pass`] up to `max_passes` times.
    ///
    /// **M2a schedule**: stop when the board is complete, when a pass attempted
    /// nothing, or at the cap. The release's `BoardHistory` restores and the two
    /// stagnation trackers (`batch-orchestration.md` §5.2) are M2c; they read
    /// only [`PassOutcome`] fields, so they attach here without moving code.
    pub fn run(&mut self, board: &mut Board, max_passes: i32) -> RunOutcome {
        let mut outcome = RunOutcome::default();
        let passes = max_passes.max(1);
        for pass_no in 1..=passes {
            let pass = self.autoroute_pass(board, pass_no);
            let done = pass.board_complete || !pass.progressed;
            outcome.incomplete_count = pass.incomplete_count;
            outcome.inserted_count += pass.routed;
            outcome.passes.push(pass);
            if done {
                break;
            }
        }
        outcome
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
        let outcome = route_with_control(&mut fixture.board, &rules, ctrl, &pads[..1], &pads[1..2]);
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
}
