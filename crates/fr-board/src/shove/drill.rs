//! `board/MoveDrillItemAlgo.java` (247 L) — translating a via or pin by
//! shoving whatever is in the way, and the via cascade that
//! [`ShoveInsert::insert`](super::ShoveInsert::insert) and
//! [`forced_pad`](super::ShoveInsert::forced_pad) both start with.

use fr_geometry::planar::{IntPoint, IntVector, Point, Simplex};

use super::entries::{tree_shape_on_layer, ShapeTraceEntries};
use super::from_side::FromSide;
use super::geom;
use super::pad::CheckDrillResult;
use super::{ShoveCheck, ShoveInsert};
use crate::board::Board;
use crate::clearance::ClearanceSource;
use crate::item::{ItemId, ItemKind};
use crate::net_nos::NetNos;
use crate::shape::{AngleRestriction, TileShape};

/// `MoveDrillItemAlgo.try_shove_via_points` (`:187-247`): candidate new centres
/// for pushing a via out of `obstacle_shape`.
///
/// `extended_check` widens the candidate set from 1 to 2 (90°) or 4 (octagon
/// or general simplex), which is what the instance check uses and the static
/// check does not.
pub fn try_shove_via_points(
    board: &Board,
    obstacle_shape: &Simplex,
    layer: i32,
    via_id: ItemId,
    cl_class_no: i32,
    extended_check: bool,
) -> Vec<IntPoint> {
    let Some(via_shape) = tree_shape_on_layer(board, via_id, layer) else {
        return Vec::new();
    };
    let mut curr_via_shape = geom::tile_to_simplex(&via_shape);
    let Some(via_item) = board.store().get(via_id) else {
        return Vec::new();
    };
    let ItemKind::Via(via) = &via_item.kind else {
        return Vec::new();
    };
    let curr_via_center = via.center;

    let is_int_octagon = obstacle_shape.is_int_octagon();
    let orthogonal = board.angle_restriction() == AngleRestriction::Ninety;
    let compensation_used = board.trees().clearance_compensation_used();
    let clearance_value = board
        .clearances()
        .value(cl_class_no, via_item.clearance_class, layer) as f64;

    let mut shove_distance;
    if orthogonal || is_int_octagon {
        // The measure is the *via* box, not the obstacle box (`:198`).
        shove_distance = 0.5 * curr_via_shape.bounding_box().max_width();
        if !compensation_used {
            shove_distance += clearance_value;
        }
    } else {
        // A different algorithm is used below; enlarge both shapes by half the
        // clearance "to synchronize with the check algorithm in
        // ShapeSearchTree.overlapping_tree_entries_with_clearance" (`:203-207`).
        shove_distance = 0.0;
        if !compensation_used {
            shove_distance += 0.5 * clearance_value;
        }
    }
    // "The additional constant 2 is an empirical value for the tolerance in
    // case of diagonal shoving" (`:211-213`).
    shove_distance += 2.0;

    if orthogonal {
        let offset_box = obstacle_shape.bounding_box().offset(shove_distance);
        let try_count = if extended_check { 2 } else { 1 };
        geom::box_nearest_border_projections(&offset_box, &curr_via_center, try_count)
    } else if is_int_octagon {
        let Some(octagon) = obstacle_shape.bounding_octagon() else {
            return Vec::new();
        };
        let offset_octagon = octagon.offset(shove_distance);
        let try_count = if extended_check { 4 } else { 1 };
        geom::octagon_nearest_border_projections(&offset_octagon, &curr_via_center, try_count)
    } else {
        let offset_shape = obstacle_shape.enlarge(shove_distance);
        if !compensation_used {
            curr_via_shape = curr_via_shape.enlarge(0.5 * clearance_value);
        }
        let try_count = if extended_check { 4 } else { 1 };
        geom::nearest_relative_outside_locations(&offset_shape, &curr_via_shape, try_count)
            .into_iter()
            .map(|delta| {
                let rounded = delta.round();
                IntPoint::new(curr_via_center.x + rounded.x, curr_via_center.y + rounded.y)
            })
            .collect()
    }
}

impl ShoveCheck<'_> {
    /// `MoveDrillItemAlgo.check(drill_item, vector, …)` (`:26-76`).
    ///
    /// `ignore_items` is upstream's shared collection: it deliberately
    /// accumulates across the recursion so a cascade ignores every via already
    /// scheduled to move.
    pub fn check_move_drill_item(
        &mut self,
        drill_item: ItemId,
        vector: &IntVector,
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        ignore_items: &mut Vec<ItemId>,
    ) -> bool {
        let board = self.board;
        let Some(item) = board.store().get(drill_item) else {
            return false;
        };
        if item.is_shove_fixed() {
            return false;
        }
        let (center, attach_allowed) = match &item.kind {
            ItemKind::Via(via) => (via.center, via.attach_allowed),
            ItemKind::Pin(pin) => (pin.center, false),
            _ => return false,
        };
        // "vias connected to other vias or pins can't be moved" (`:38-44`).
        let mut contacts = Vec::new();
        board.normal_contacts_at(drill_item, &Point::Int(center), &mut contacts);
        for &contact_id in contacts.iter() {
            let Some(contact) = board.store().get(contact_id) else {
                continue;
            };
            if !matches!(
                contact.kind,
                ItemKind::Trace(_) | ItemKind::ConductionArea(_)
            ) {
                return false;
            }
        }
        if !ignore_items.contains(&drill_item) {
            ignore_items.push(drill_item);
        }

        let nets = item.nets.clone();
        let cl_class = item.clearance_class;
        let layers = item.layer_range();
        let orthogonal = self.orthogonal;
        for layer in layers.first..=layers.last {
            let Some(curr_shape) = tree_shape_on_layer(board, drill_item, layer) else {
                continue;
            };
            let new_shape = curr_shape.translate_by(&fr_geometry::planar::Vector::Int(*vector));
            let curr_tile_shape = drill_tile_shape(&new_shape, orthogonal);
            let from_side =
                FromSide::from_point(&Point::Int(center), &curr_tile_shape.to_simplex());
            if self.check_forced_pad(
                &curr_tile_shape,
                Some(from_side),
                layer,
                &nets,
                cl_class,
                attach_allowed,
                ignore_items,
                max_recursion_depth,
                max_via_recursion_depth,
                true,
            ) == CheckDrillResult::NotDrillable
            {
                return false;
            }
        }
        true
    }
}

impl ShoveInsert<'_> {
    /// `MoveDrillItemAlgo.insert(drill_item, vector, …)` (`:82-123`).
    pub fn move_drill_item(
        &mut self,
        drill_item: ItemId,
        vector: &IntVector,
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
    ) -> bool {
        let Some(item) = self.board.store().get(drill_item) else {
            return false;
        };
        if item.is_shove_fixed() {
            return false;
        }
        let (center, attach_allowed) = match &item.kind {
            ItemKind::Via(via) => (via.center, via.attach_allowed),
            ItemKind::Pin(pin) => (pin.center, false),
            _ => return false,
        };
        let nets = item.nets.clone();
        let cl_class = item.clearance_class;
        let layers = item.layer_range();
        let ignore_items = vec![drill_item];
        let orthogonal = self.orthogonal;

        for layer in layers.first..=layers.last {
            let Some(curr_shape) = tree_shape_on_layer(self.board, drill_item, layer) else {
                continue;
            };
            let new_shape = curr_shape.translate_by(&fr_geometry::planar::Vector::Int(*vector));
            let curr_tile_shape = drill_tile_shape(&new_shape, orthogonal).to_simplex();
            let from_side = FromSide::from_point(&Point::Int(center), &curr_tile_shape);
            if !self.forced_pad(
                &curr_tile_shape,
                Some(from_side),
                layer,
                &nets,
                cl_class,
                attach_allowed,
                &ignore_items,
                max_recursion_depth,
                max_via_recursion_depth,
            ) {
                return false;
            }
            let curr_bounding_box = geom::tile_to_simplex(&curr_shape).bounding_box();
            for j in 0..4 {
                self.changed
                    .join_point(curr_bounding_box.corner(j).to_float(), layer);
            }
        }
        self.board.translate_drill_item(drill_item, vector)
    }

    /// `MoveDrillItemAlgo.shove_vias(obstacle_shape, …)` (`:128-181`).
    ///
    /// Returns false **only** when an `insert` failed — "no candidate" simply
    /// leaves the via in place, to be re-detected and failed by the caller.
    #[allow(clippy::too_many_arguments)]
    pub fn shove_vias(
        &mut self,
        obstacle_shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
        net_no_arr: &NetNos,
        cl_type: i32,
        ignore_items: &[ItemId],
        max_recursion_depth: i32,
        max_via_recursion_depth: i32,
        copper_sharing_allowed: bool,
    ) -> bool {
        let mut shape_entries = ShapeTraceEntries::new(
            obstacle_shape.clone(),
            layer,
            net_no_arr.clone(),
            cl_type,
            from_side,
        );
        let mut obstacles = self.scratch.take();
        geom::overlapping_items_with_clearance(
            self.board,
            obstacle_shape,
            layer,
            &[],
            cl_type,
            &mut obstacles,
        );
        let stored =
            shape_entries.store_items(self.board, &obstacles, false, copper_sharing_allowed);
        self.scratch.give(obstacles);
        if !stored {
            return true;
        }
        shape_entries
            .shove_via_list
            .retain(|id| !ignore_items.contains(id));
        if shape_entries.shove_via_list.is_empty() {
            return true;
        }
        let shape_radius = 0.5 * obstacle_shape.bounding_box().min_width();
        let via_list = shape_entries.shove_via_list.clone();
        for via_id in via_list {
            let Some(via_item) = self.board.store().get(via_id) else {
                continue;
            };
            if via_item.nets.shares_any(net_no_arr) {
                continue;
            }
            if max_via_recursion_depth <= 0 {
                return true;
            }
            let ItemKind::Via(via) = &via_item.kind else {
                continue;
            };
            let curr_via_center = via.center;
            let check_via_center = curr_via_center.to_float();
            let max_dist = 0.5
                * tree_shape_on_layer(self.board, via_id, layer)
                    .map(|shape| shape.bounding_box())
                    .unwrap_or(fr_geometry::planar::IntBox::EMPTY)
                    .max_width()
                + shape_radius;
            let max_dist_square = max_dist * max_dist;
            let try_via_centers =
                try_shove_via_points(self.board, obstacle_shape, layer, via_id, cl_type, true);
            let mut chosen: Option<IntVector> = None;
            for (i, candidate) in try_via_centers.iter().enumerate() {
                if i != 0
                    && check_via_center.distance_square(&candidate.to_float()) > max_dist_square
                {
                    continue;
                }
                let rel_coor = candidate.difference_by_int(&curr_via_center);
                let mut cascade_ignore = ignore_items.to_vec();
                // "No time limit here because the item database is already
                // changed" (`:165`).
                let ok = {
                    let mut checker = self.checker();
                    checker.check_move_drill_item(
                        via_id,
                        &rel_coor,
                        max_recursion_depth,
                        max_via_recursion_depth - 1,
                        &mut cascade_ignore,
                    )
                };
                if ok {
                    chosen = Some(rel_coor);
                    break;
                }
            }
            let Some(rel_coor) = chosen else {
                continue;
            };
            if !self.move_drill_item(
                via_id,
                &rel_coor,
                max_recursion_depth,
                max_via_recursion_depth - 1,
            ) {
                return false;
            }
        }
        true
    }
}

/// The snap upstream applies to a translated drill shape: the bounding box at
/// 90°, the bounding octagon otherwise (`:63-68`, `:100-105`).
fn drill_tile_shape(shape: &crate::shape::TileShape, orthogonal: bool) -> TileShape {
    if orthogonal {
        TileShape::Box(shape.bounding_box())
    } else {
        match shape {
            TileShape::Box(b) => TileShape::Box(*b),
            TileShape::Oct(o) => TileShape::Oct(*o),
            TileShape::Simplex(s) => match s.bounding_octagon() {
                Some(octagon) => TileShape::Oct(octagon),
                None => TileShape::Simplex(s.clone()),
            },
        }
    }
}
