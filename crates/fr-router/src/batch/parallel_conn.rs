//! T3: Parallel Connection Routing.
//!
//! Optimistic concurrent maze search on board snapshots with deterministic in-order
//! commit and transactional correctness verification.
//!
//! See `docs/upstream-study/parallel-connections.md` for the full design and contract.

use fr_board::board::Board;
use fr_board::item::{Item, ItemId, ItemKind};
use fr_geometry::planar::{IntBox, IntPoint};
use fr_rules::board_rules::BoardRules;
use fr_rules::router_settings::RouterSettings;

use crate::batch::budget::{ConnectionBudget, ConnectionUsage};
use crate::batch::{
    connected_set, unconnected_set, AttemptOutcome, AttemptState, ContactScratch, WorkItem,
};
use crate::ctrl::AutorouteControl;
use crate::engine::AutorouteEngine;
use crate::locate::{locate, LocatedConnection};
use crate::maze::search::MazeSearch;

/// A candidate routing attempt evaluated by a parallel worker on a board clone.
#[derive(Debug)]
pub struct CandidateAttempt {
    /// The work item that was attempted.
    pub work_item: WorkItem,
    /// The attempt state (e.g. Routed, NotRouted, Skipped).
    pub state: AttemptState,
    /// The attempt outcome details.
    pub outcome: AttemptOutcome,
    /// The located connection geometry if found.
    pub located: Option<LocatedConnection>,
    /// Engine spatial bounds during search.
    pub bounds: IntBox,
    /// The autoroute control used for the search.
    pub ctrl: Option<AutorouteControl>,
    /// Segment-level read/search footprint bounding boxes.
    pub read_boxes: Vec<IntBox>,
    /// Segment-level write footprint bounding boxes.
    pub write_boxes: Vec<IntBox>,
    /// New/touched item IDs on the worker board.
    pub touched_items: Vec<ItemId>,
    /// Whether any copper was ripped up during the attempt.
    pub ripped: bool,
    /// Resource usage (time and effort).
    pub usage: ConnectionUsage,
    /// First new item ID allocated on the worker board.
    pub first_new_id: ItemId,
}

fn max_clearance(rules: &BoardRules) -> f64 {
    (0..rules.layer_structure.len())
        .map(|l| rules.clearance_matrix.max_value_on_layer(l))
        .max()
        .unwrap_or(0) as f64
}

/// Computes a conservative 2D bounding box for an item.
pub fn item_bounding_box(item: &Item) -> IntBox {
    match &item.kind {
        ItemKind::Pin(pin) => IntBox::new(
            pin.center.x - 500,
            pin.center.y - 500,
            pin.center.x + 500,
            pin.center.y + 500,
        ),
        ItemKind::Via(via) => IntBox::new(
            via.center.x - 500,
            via.center.y - 500,
            via.center.x + 500,
            via.center.y + 500,
        ),
        ItemKind::Trace(trace) => {
            let hw = trace.half_width as f64;
            trace.polyline.bounding_box().offset(hw)
        }
        _ => IntBox::EMPTY,
    }
}

fn drill_center(board: &Board, id: ItemId) -> Option<IntPoint> {
    match &board.store().get(id)?.kind {
        ItemKind::Pin(pin) => Some(pin.center),
        ItemKind::Via(via) => Some(via.center),
        _ => None,
    }
}

fn pin_ends_to_connection_points(board: &Board, located: &mut LocatedConnection) {
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

/// Evaluates one connection attempt on a worker board clone, rolling back any mutations
/// before returning so the board is clean for subsequent evaluations.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_candidate(
    board: &mut Board,
    rules: &BoardRules,
    settings: &RouterSettings,
    entry: WorkItem,
    ripup_costs: i32,
    remove_unconnected_vias: bool,
    budget_budget: ConnectionBudget,
    scratch: &mut ContactScratch,
) -> CandidateAttempt {
    let first_new_id = board.store().next_id();
    let mut clock = budget_budget.start();

    let contains_plane = rules
        .nets
        .get(entry.net_no)
        .is_some_and(fr_rules::net::Net::contains_plane);

    let connected = connected_set(board, entry.net_no, entry.item, scratch);
    let unconnected = unconnected_set(board, entry.net_no, &connected);

    if unconnected.is_empty() {
        let usage = clock.finish();
        return CandidateAttempt {
            work_item: entry,
            state: AttemptState::NoUnconnectedNets,
            outcome: AttemptOutcome::of(AttemptState::NoUnconnectedNets),
            located: None,
            bounds: IntBox::EMPTY,
            ctrl: None,
            read_boxes: Vec::new(),
            write_boxes: Vec::new(),
            touched_items: Vec::new(),
            ripped: false,
            usage,
            first_new_id,
        };
    }

    if contains_plane
        && connected.iter().any(|member| {
            matches!(
                board.store().get(*member).map(|i| &i.kind),
                Some(ItemKind::ConductionArea(_))
            )
        })
    {
        let usage = clock.finish();
        return CandidateAttempt {
            work_item: entry,
            state: AttemptState::ConnectedToPlane,
            outcome: AttemptOutcome::of(AttemptState::ConnectedToPlane),
            located: None,
            bounds: IntBox::EMPTY,
            ctrl: None,
            read_boxes: Vec::new(),
            write_boxes: Vec::new(),
            touched_items: Vec::new(),
            ripped: false,
            usage,
            first_new_id,
        };
    }

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
    let mut ctrl =
        AutorouteControl::for_net_with_costs(board, rules, settings, entry.net_no, via_costs);
    ctrl.ripup_allowed = true;
    ctrl.ripup_costs = ripup_costs;
    ctrl.remove_unconnected_vias = remove_unconnected_vias;

    board.ensure_compensated_tree(ctrl.trace_clearance_class_no);

    // Compute start & destination item boxes
    let mut endpoint_boxes = Vec::new();
    for &id in start.iter().chain(destination.iter()) {
        if let Some(item) = board.store().get(id) {
            endpoint_boxes.push(item_bounding_box(item));
        }
    }

    let (located_res, bounds) = {
        let mut engine = AutorouteEngine::new(board, rules, ctrl.clone());
        engine.set_clock(&mut clock);
        let bounds = engine.bounds();
        let connection = match MazeSearch::get_instance(&mut engine, &start, &destination) {
            Ok(mut search) => search.find_connection().ok(),
            Err(_) => None,
        };
        match connection {
            Some(conn) => {
                let start_layer = conn.start_layer().unwrap_or(0);
                let target_layer = conn.target_layer().unwrap_or(0);
                if !ctrl.layer_is_active(start_layer) || !ctrl.layer_is_active(target_layer) {
                    (None, bounds)
                } else {
                    match locate(&engine, &conn) {
                        Ok(mut located) => {
                            pin_ends_to_connection_points(engine.board, &mut located);
                            (Some(located), bounds)
                        }
                        Err(_) => (None, bounds),
                    }
                }
            }
            None => (None, bounds),
        }
    };

    let max_hw = ctrl.trace_half_width.iter().copied().max().unwrap_or(1000) as f64;
    let clearance_margin = max_clearance(rules) + max_hw;

    let Some(located) = located_res else {
        let usage = clock.finish();
        let read_boxes: Vec<IntBox> = endpoint_boxes
            .into_iter()
            .map(|b| b.offset(clearance_margin))
            .collect();
        return CandidateAttempt {
            work_item: entry,
            state: AttemptState::NotRouted,
            outcome: AttemptOutcome::of(AttemptState::NotRouted),
            located: None,
            bounds,
            ctrl: Some(ctrl),
            read_boxes,
            write_boxes: Vec::new(),
            touched_items: Vec::new(),
            ripped: false,
            usage,
            first_new_id,
        };
    };

    // Calculate segment-level read and write footprint boxes
    let mut read_boxes = Vec::new();
    let mut write_boxes = Vec::new();

    for b in endpoint_boxes {
        read_boxes.push(b.offset(clearance_margin));
    }

    for item in &located.connection_items {
        for w in item.corners.windows(2) {
            let seg_box = IntBox::new_from_points(w[0], w[1]).offset(clearance_margin);
            read_boxes.push(seg_box);
            write_boxes.push(seg_box);
        }
        if item.corners.len() == 1 {
            let p = item.corners[0];
            let pt_box = IntBox::new(p.x, p.y, p.x, p.y).offset(clearance_margin);
            read_boxes.push(pt_box);
            write_boxes.push(pt_box);
        }
    }

    let mut ripped: Vec<ItemId> = located.ripped_items.iter().map(|item| item.item).collect();
    ripped.sort_unstable();
    ripped.dedup();
    let has_ripped = !ripped.is_empty();

    let mut outcome = AttemptOutcome {
        ripped,
        ..AttemptOutcome::default()
    };
    outcome.state = Some(AttemptState::Routed);

    let usage = clock.finish();

    CandidateAttempt {
        work_item: entry,
        state: AttemptState::Routed,
        outcome,
        located: Some(located),
        bounds,
        ctrl: Some(ctrl),
        read_boxes,
        write_boxes,
        touched_items: Vec::new(),
        ripped: has_ripped,
        usage,
        first_new_id,
    }
}
