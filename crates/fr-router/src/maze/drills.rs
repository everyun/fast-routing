//! Layer changes: `ExpansionDrill`, `DrillPage`, `DrillPageArray`.
//!
//! Transcribed from `autoroute/ExpansionDrill.java`, `autoroute/DrillPage.java`
//! and `autoroute/DrillPageArray.java`. The page grid is the cache that keeps a
//! big room from enumerating thousands of via sites: a room expands to the
//! *pages* it overlaps, and only a popped page enumerates its drills
//! (`maze-engine.md` §7.2-7.3, the three-hop via chain).
//!
//! Conformance appendix D15: `max(5 × default_via_diameter, 10000)` is the
//! *maximum* page width fed to the grid; the actual page size is
//! `ceil(span / ceil(span / max_page_width))`, with the last column and row
//! extended to the bounding-box edge.
//!
//! Conformance appendix D9: the obstacle source is the **net-compensated**
//! autoroute tree, not the default board tree, so page cutouts already carry
//! the routed net's clearance.

use fr_board::item::{ItemClass, ItemId};
use fr_geometry::planar::{IntBox, IntPoint, Point, Simplex};

use crate::engine::AutorouteEngine;
use crate::geom;
use crate::maze::nodes::{NodeArena, NodeIndex};
use crate::rooms::{DrillId, PageId, RoomId};

/// A candidate via site (`ExpansionDrill`).
#[derive(Debug, Clone)]
pub struct ExpansionDrill {
    /// Monotonic object id — the queue tie-break and the settle-trace key.
    pub oid: u32,
    /// The convex free area the via must fit in.
    pub shape: Simplex,
    /// The drill point.
    pub location: IntPoint,
    /// First layer the drill spans.
    pub first_layer: i32,
    /// Last layer the drill spans.
    pub last_layer: i32,
    /// The room containing `location` on each layer of the span.
    pub room_arr: Vec<RoomId>,
    /// Base of this drill's per-layer node block.
    node_base: NodeIndex,
}

impl ExpansionDrill {
    /// Number of layers, i.e. the number of "sections" of this expandable
    /// object (`maze_search_element_count`, `:111-113`).
    #[inline]
    pub fn section_count(&self) -> u16 {
        (self.last_layer - self.first_layer + 1) as u16
    }

    /// The node index of one layer of the drill.
    #[inline]
    pub fn section_node(&self, section: u16) -> Option<NodeIndex> {
        (section < self.section_count()).then(|| NodeIndex(self.node_base.0 + section as u32))
    }
}

/// The drill arena — drills are created by [`AutorouteEngine::page_drills`] and
/// live until the engine is cleared.
#[derive(Debug, Default)]
pub struct DrillArena {
    drills: Vec<ExpansionDrill>,
}

impl DrillArena {
    /// An empty arena.
    pub fn new() -> DrillArena {
        DrillArena::default()
    }

    /// Drops every drill.
    pub fn clear(&mut self) {
        self.drills.clear();
    }

    /// Number of drills created so far.
    #[inline]
    pub fn len(&self) -> usize {
        self.drills.len()
    }

    /// True when no drill has been created.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.drills.is_empty()
    }

    /// Read access to a drill.
    #[inline]
    pub fn get(&self, id: DrillId) -> &ExpansionDrill {
        &self.drills[id.0 as usize]
    }

    /// Appends a drill.
    pub fn push(&mut self, drill: ExpansionDrill) -> DrillId {
        let id = DrillId(self.drills.len() as u32);
        self.drills.push(drill);
        id
    }
}

/// One rectangular page of the drill grid (`DrillPage`).
#[derive(Debug, Clone)]
pub struct DrillPage {
    /// Monotonic object id.
    pub oid: u32,
    /// The page rectangle.
    pub shape: IntBox,
    /// Base of the per-layer node block; a page's "section" index is the layer
    /// (conformance appendix §4(d), `DrillPage.java:23`).
    node_base: Option<NodeIndex>,
    /// The drills of this page, `None` until calculated.
    drills: Option<Vec<DrillId>>,
    /// The net the cached drills were calculated for; `-1` = never.
    net_no: i32,
}

impl DrillPage {
    /// The cached drills, if any.
    #[inline]
    pub fn cached_drills(&self) -> Option<&[DrillId]> {
        self.drills.as_deref()
    }

    /// `DrillPage.invalidate()` (`:167-169`): drop the cache, keep the nodes.
    #[inline]
    pub fn invalidate(&mut self) {
        self.drills = None;
    }
}

/// The 2-D grid of drill pages (`DrillPageArray`).
#[derive(Debug)]
pub struct DrillPageArray {
    bounds: IntBox,
    column_count: i32,
    row_count: i32,
    page_width: i32,
    page_height: i32,
    layer_count: usize,
    pages: Vec<DrillPage>,
    next_oid: u32,
}

impl DrillPageArray {
    /// `DrillPageArray(board, max_page_width)` (`:38-66`).
    pub fn new(bounds: &IntBox, max_page_width: i32, layer_count: usize) -> DrillPageArray {
        let length = (bounds.right_x() - bounds.left_x()) as f64;
        let height = (bounds.top_y() - bounds.bottom_y()) as f64;
        let max_page_width = max_page_width.max(1) as f64;
        let column_count = ((length / max_page_width).ceil() as i32).max(1);
        let row_count = ((height / max_page_width).ceil() as i32).max(1);
        let page_width = ((length / column_count as f64).ceil() as i32).max(1);
        let page_height = ((height / row_count as f64).ceil() as i32).max(1);

        let mut pages = Vec::with_capacity((row_count * column_count) as usize);
        let mut next_oid = 0u32;
        for j in 0..row_count {
            for i in 0..column_count {
                let ll_x = bounds.left_x() + i * page_width;
                let ur_x = if i == column_count - 1 {
                    bounds.right_x()
                } else {
                    ll_x + page_width
                };
                let ll_y = bounds.bottom_y() + j * page_height;
                let ur_y = if j == row_count - 1 {
                    bounds.top_y()
                } else {
                    ll_y + page_height
                };
                pages.push(DrillPage {
                    oid: next_oid,
                    shape: IntBox::new(ll_x, ll_y, ur_x, ur_y),
                    node_base: None,
                    drills: None,
                    net_no: -1,
                });
                next_oid += 1;
            }
        }
        DrillPageArray {
            bounds: *bounds,
            column_count,
            row_count,
            page_width,
            page_height,
            layer_count,
            pages,
            next_oid,
        }
    }

    /// The object-id space the pages consumed; the engine's door/drill ids are
    /// offset past it so every expandable object has a distinct key.
    #[inline]
    pub fn oid_ceiling(&self) -> u32 {
        self.next_oid
    }

    /// Number of pages.
    #[inline]
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// True when the grid is degenerate.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Read access to a page.
    #[inline]
    pub fn page(&self, id: PageId) -> &DrillPage {
        &self.pages[id.0 as usize]
    }

    /// Write access to a page.
    #[inline]
    pub fn page_mut(&mut self, id: PageId) -> &mut DrillPage {
        &mut self.pages[id.0 as usize]
    }

    /// `overlapping_pages(shape)` (`:82-102`).
    ///
    /// `min_i`/`min_j` are floors, `max_i`/`max_j` stay `double` and are
    /// compared with `<` — that asymmetry is upstream's and decides whether a
    /// shape ending exactly on a page border touches the next page.
    pub fn overlapping_pages(&self, shape: &Simplex, out: &mut Vec<PageId>) {
        out.clear();
        if !shape.is_bounded() {
            out.extend((0..self.pages.len() as u32).map(PageId));
            return;
        }
        let shape_box = shape.bounding_box().intersection(&self.bounds);
        if shape_box.is_empty() {
            return;
        }
        let min_j = (((shape_box.bottom_y() - self.bounds.bottom_y()) as f64)
            / self.page_height as f64)
            .floor() as i32;
        let max_j = ((shape_box.top_y() - self.bounds.bottom_y()) as f64) / self.page_height as f64;
        let min_i = (((shape_box.left_x() - self.bounds.left_x()) as f64) / self.page_width as f64)
            .floor() as i32;
        let max_i = ((shape_box.right_x() - self.bounds.left_x()) as f64) / self.page_width as f64;

        let mut j = min_j.max(0);
        while (j as f64) < max_j && j < self.row_count {
            let mut i = min_i.max(0);
            while (i as f64) < max_i && i < self.column_count {
                let id = PageId((j * self.column_count + i) as u32);
                let page_shape = self.pages[id.0 as usize].shape.to_simplex();
                if shape.intersection(&page_shape).dimension() > 1 {
                    out.push(id);
                }
                i += 1;
            }
            j += 1;
        }
    }

    /// `DrillPageArray.invalidate(shape)` (`:106-115`).
    pub fn invalidate(&mut self, shape: &Simplex) {
        let mut hits = Vec::new();
        self.overlapping_pages(shape, &mut hits);
        for id in hits {
            self.page_mut(id).invalidate();
        }
    }

    /// `DrillPageArray.reset()` (`:70-77`): reset every page's maze elements
    /// *and* every cached drill's — but keep the drills themselves
    /// (`DrillPage.reset`, `:154-164`). Here the node reset is the arena's
    /// generation bump, so this only has to keep the caches.
    pub fn reset_all(&mut self) {
        for page in &mut self.pages {
            page.node_base = None;
            page.net_no = -1;
            page.drills = None;
        }
    }

    /// Ensures the page's per-layer node block exists and returns its base.
    fn ensure_nodes(&mut self, id: PageId, nodes: &mut NodeArena) -> NodeIndex {
        let layer_count = self.layer_count;
        let page = self.page_mut(id);
        match page.node_base {
            Some(base) => base,
            None => {
                let base = nodes.allocate(layer_count);
                page.node_base = Some(base);
                base
            }
        }
    }

    /// The node index of `(page, layer)`.
    pub fn page_node(
        &mut self,
        id: PageId,
        layer: i32,
        nodes: &mut NodeArena,
    ) -> Option<NodeIndex> {
        if layer < 0 || layer as usize >= self.layer_count {
            return None;
        }
        let base = self.ensure_nodes(id, nodes);
        Some(NodeIndex(base.0 + layer as u32))
    }
}

impl AutorouteEngine<'_> {
    /// `DrillPage.get_drills(engine, attach_smd)` (`:62-135`), cached per
    /// `(page, net)`.
    pub fn page_drills(&mut self, page_id: PageId, attach_smd: bool) -> Vec<DrillId> {
        let net_no = self.ctrl.net_no;
        {
            let page = self.pages.page(page_id);
            if let Some(drills) = page.cached_drills() {
                if page.net_no == net_no {
                    return drills.to_vec();
                }
            }
        }
        let page_box = self.pages.page(page_id).shape;
        let page_shape = page_box.to_simplex();

        // Step 1-2: cut every non-drillable obstacle out of the page box.
        let mut cutouts: Vec<Simplex> = Vec::new();
        let mut entries = Vec::new();
        self.overlapping_items_all_layers(&page_shape, &mut entries);
        let mut prev_obstacle_shape: Option<Simplex> = None;
        for (item_id, index) in entries {
            let Some(item) = self.board.store().get(item_id) else {
                continue;
            };
            if item.is_drillable(net_no) {
                continue;
            }
            if item.class() == ItemClass::Pin && attach_smd && item.drill_allowed() {
                continue;
            }
            let Some(obstacle_shape) = self.item_tile(item_id, index) else {
                continue;
            };
            // "Checked to avoid multiple cutout for example for vias with the
            // same shape on all layers" (`:87-89`).
            let repeated = prev_obstacle_shape
                .as_ref()
                .map(|prev| geom::contains_shape(prev, &obstacle_shape))
                .unwrap_or(false);
            if !repeated {
                let cutout = obstacle_shape.intersection(&page_shape);
                if cutout.dimension() == 2 {
                    cutouts.push(cutout);
                }
            }
            prev_obstacle_shape = Some(obstacle_shape);
        }

        // Step 2 (continued): split the rest into convex pieces.
        //
        // Upstream builds a `PolylineArea(page, holes)` and calls
        // `split_to_convex`. `fr-geometry` has no `PolylineArea`, so the same
        // negative space is produced by repeated `Simplex::cutout`, which
        // subtracts one convex hole from a convex piece and returns the convex
        // remainder. The *set* of covered points is identical; the individual
        // pieces are not, so drill locations (piece centres of gravity) differ
        // from Java's on any page that has more than one hole.
        let mut pieces = vec![page_shape.clone()];
        for hole in &cutouts {
            let mut next = Vec::with_capacity(pieces.len());
            for piece in &pieces {
                if piece.intersection(hole).dimension() < 2 {
                    next.push(piece.clone());
                    continue;
                }
                next.extend(
                    piece
                        .cutout(hole)
                        .into_iter()
                        .filter(|part| part.dimension() == 2),
                );
            }
            pieces = next;
            if pieces.is_empty() {
                break;
            }
        }

        // Step 3: one drill per convex piece, spanning all layers.
        let drill_first_layer = 0i32;
        let drill_last_layer = self.ctrl.layer_count as i32 - 1;
        let mut result = Vec::new();
        for piece in pieces {
            let mut location = None;
            if attach_smd {
                location = self.pin_center_in_drill(&piece, drill_first_layer);
                if location.is_none() {
                    location = self.pin_center_in_drill(&piece, drill_last_layer);
                }
            }
            let location = location.unwrap_or_else(|| piece.centre_of_gravity().round());
            let oid = self.rooms.next_object_id();
            let node_base = self
                .nodes
                .allocate((drill_last_layer - drill_first_layer + 1) as usize);
            let mut drill = ExpansionDrill {
                oid,
                shape: piece,
                location,
                first_layer: drill_first_layer,
                last_layer: drill_last_layer,
                room_arr: Vec::new(),
                node_base,
            };
            if self.calculate_drill_expansion_rooms(&mut drill) {
                result.push(self.drills.push(drill));
            }
        }

        let page = self.pages.page_mut(page_id);
        page.net_no = net_no;
        page.drills = Some(result.clone());
        result
    }

    /// `ExpansionDrill.calculate_expansion_rooms` (`:59-93`): point-query for a
    /// complete room on each layer, completing a fresh one where there is none.
    /// Anything but exactly one completed room means an obstacle sits at the
    /// drill point, and the drill is discarded.
    fn calculate_drill_expansion_rooms(&mut self, drill: &mut ExpansionDrill) -> bool {
        let search_shape = Point::Int(drill.location)
            .surrounding_octagon()
            .to_simplex();
        drill.room_arr.clear();
        for layer in drill.first_layer..=drill.last_layer {
            let existing = self.room_at_point(&search_shape, layer);
            let room = match existing {
                Some(room) => room,
                None => {
                    let incomplete =
                        self.rooms
                            .add_incomplete_room(None, layer, search_shape.clone());
                    let completed = self.complete_expansion_room(incomplete);
                    if completed.len() != 1 {
                        return false;
                    }
                    completed[0]
                }
            };
            drill.room_arr.push(room);
        }
        true
    }

    /// The completed free room containing `shape` on `layer`, if any.
    fn room_at_point(&mut self, shape: &Simplex, layer: i32) -> Option<RoomId> {
        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        self.overlapping_obstacles(
            shape,
            Some(layer),
            self.ctrl.net_no,
            &mut obstacles,
            &mut own_net,
        );
        obstacles.iter().find_map(|obstacle| match obstacle {
            crate::engine::Obstacle::Room(room) => Some(*room),
            _ => None,
        })
    }

    /// `DrillPage.calc_pin_center_in_drill` (`:46-61`): the centre of a
    /// drillable pin strictly inside the drill shape, if there is one.
    fn pin_center_in_drill(&mut self, drill_shape: &Simplex, layer: i32) -> Option<IntPoint> {
        let mut entries = Vec::new();
        self.overlapping_items_on_layer(drill_shape, Some(layer), &mut entries);
        let mut result = None;
        for (item_id, _) in entries {
            let Some(item) = self.board.store().get(item_id) else {
                continue;
            };
            let fr_board::item::ItemKind::Pin(pin) = &item.kind else {
                continue;
            };
            if item.drill_allowed()
                && fr_geometry::planar::Shape::contains_inside(drill_shape, &Point::Int(pin.center))
            {
                result = Some(pin.center);
            }
        }
        result
    }

    /// Every item tile overlapping `query`, on all layers, ascending id.
    fn overlapping_items_all_layers(&mut self, query: &Simplex, out: &mut Vec<(ItemId, u16)>) {
        self.overlapping_items_on_layer(query, None, out);
    }

    /// Every item tile overlapping `query` on `layer`, ascending id then index.
    pub(crate) fn overlapping_items_on_layer(
        &mut self,
        query: &Simplex,
        layer: Option<i32>,
        out: &mut Vec<(ItemId, u16)>,
    ) {
        let mut obstacles = Vec::new();
        let mut own_net = Vec::new();
        // Net 0 is never an own net, so everything lands in `obstacles` and the
        // caller sees the unfiltered set.
        self.overlapping_obstacles(query, layer, 0, &mut obstacles, &mut own_net);
        out.clear();
        out.extend(obstacles.into_iter().filter_map(|obstacle| match obstacle {
            crate::engine::Obstacle::Item(item, index) => Some((item, index)),
            crate::engine::Obstacle::Room(_) => None,
        }));
        out.extend(own_net);
        out.sort_unstable();
        out.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_covers_the_bounds_exactly() {
        let bounds = IntBox::new(0, 0, 25_000, 9_000);
        let grid = DrillPageArray::new(&bounds, 10_000, 2);
        // ceil(25000/10000) = 3 columns, ceil(9000/10000) = 1 row.
        assert_eq!(grid.len(), 3);
        // The last column runs to the bounding-box edge (D15).
        assert_eq!(grid.page(PageId(2)).shape.right_x(), 25_000);
        assert_eq!(grid.page(PageId(0)).shape.left_x(), 0);
        assert_eq!(grid.page(PageId(0)).shape.top_y(), 9_000);
    }

    #[test]
    fn page_size_is_the_ceil_of_the_span_over_the_column_count() {
        let bounds = IntBox::new(0, 0, 25_000, 25_000);
        let grid = DrillPageArray::new(&bounds, 10_000, 2);
        // 3 columns ⇒ page width ceil(25000/3) = 8334, not 10000.
        assert_eq!(grid.page(PageId(0)).shape.right_x(), 8_334);
    }

    #[test]
    fn overlapping_pages_needs_a_two_dimensional_overlap() {
        let bounds = IntBox::new(0, 0, 20_000, 10_000);
        let grid = DrillPageArray::new(&bounds, 10_000, 2);
        let mut out = Vec::new();
        grid.overlapping_pages(
            &IntBox::new(1_000, 1_000, 2_000, 2_000).to_simplex(),
            &mut out,
        );
        assert_eq!(out, vec![PageId(0)]);
        // A shape spanning the page border hits both.
        grid.overlapping_pages(
            &IntBox::new(9_000, 1_000, 11_000, 2_000).to_simplex(),
            &mut out,
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn invalidate_only_drops_the_cache() {
        let bounds = IntBox::new(0, 0, 20_000, 10_000);
        let mut grid = DrillPageArray::new(&bounds, 10_000, 2);
        grid.page_mut(PageId(0)).drills = Some(vec![DrillId(0)]);
        grid.page_mut(PageId(0)).net_no = 7;
        grid.invalidate(&IntBox::new(1_000, 1_000, 2_000, 2_000).to_simplex());
        assert!(grid.page(PageId(0)).cached_drills().is_none());
        assert!(grid.page(PageId(1)).cached_drills().is_none());
    }
}
