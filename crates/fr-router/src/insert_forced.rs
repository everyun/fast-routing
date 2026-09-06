//! The board primitives `InsertFoundConnectionAlgo` calls, **with shove
//! enabled** (M2b).
//!
//! Transcribed from
//! `/tmp/fr-java-release/src/main/java/app/freerouting/board/RoutingBoard.java`
//! (`insert_forced_trace_polyline` `:591-899` = spec §4.6, `connect_to_trace`
//! `:1089`, `check_trace_segment` `:232-306`), `BasicBoard.java`
//! (`check_trace_shape` `:1166`, `get_trace_tail` `:1857`),
//! `ForcedViaAlgo.java` (`check` `:89`, `insert` `:165`) and
//! `ChangedArea.java`.
//!
//! # Where the shove enters
//!
//! Every clearance question here funnels through `ShoveTraceAlgo.check` /
//! `ForcedPadAlgo.check_forced_pad`, which live in
//! [`fr_board::shove`]. This module is the call site:
//!
//! | §4.6 step | upstream | here |
//! |---|---|---|
//! | 3 | `spring_over_obstacles(polyline, …)` | [`ForcedInserter::insert_forced_trace_polyline`] |
//! | 5 | `CalcFromSide(combined_polyline, i + 1, shape)` + instance `check` | [`ForcedInserter::check_trace_shape`] |
//! | 5 | `ShoveTraceAlgo.insert(...)` | [`ForcedInserter::insert_trace_shape`] |
//! | via | `ForcedViaAlgo.check` / `insert` | [`ForcedInserter::check_via`] / [`ForcedInserter::insert_forced_via`] |
//!
//! # Rollback purity
//!
//! Upstream's `insert` *"may damage the database"*, and
//! `insert_forced_trace_polyline` reports that as a `null` ok-point which the
//! caller decodes as "undo the whole connection". Here
//! [`fr_board::ShoveInsert::insert_transaction`] rolls the cascade back itself,
//! so a failed shove leaves the board bit-identical. The `null`/`None` return
//! is still produced — `InsertFoundConnectionAlgo` branches on all three
//! ok-point cases — it just no longer implies damage, and the adversarial
//! rollback probes in `tests/` assert exactly that.
//!
//! # Budgets
//!
//! [`AutorouteControl`] carries upstream's `max_shove_trace_recursion_depth`
//! (20), `max_shove_via_recursion_depth` (5) and
//! `max_spring_over_recursion_depth` (5); they are passed straight through, and
//! the top-level `spring_over_obstacles` uses its own
//! `c_max_spring_over_recursion_depth = 20` inside `fr-board`.

use fr_board::board::Board;
use fr_board::clearance::ClearanceSource;
use fr_board::item::{FixedState, ItemId, ItemKind, LayerRange, Via};
use fr_board::net_nos::NetNos;
use fr_board::shape::{AngleRestriction, Padstack as BoardPadstack, TileShape};
use fr_board::shove::via::ViaLayerPad;
use fr_board::shove::{ChangedAreaSink, CheckDrillResult, FromSide, ShoveCheck, ShoveInsert};
use fr_geometry::planar::{
    Circle, FloatPoint, IntBox, IntOctagon, IntPoint, Point, Polyline, Simplex,
};
use fr_rules::board_rules::BoardRules;
use fr_rules::padstack::{PadShape, Padstack as RulesPadstack};
use fr_rules::via_info::ViaInfo;

use crate::batch::budget::BudgetMode;
use crate::ctrl::AutorouteControl;

/// `ChangedArea` (`board/ChangedArea.java`): one mutable double-precision
/// octagon per layer, grown by every mutation site and read as the normalize
/// clip and (in M2b) as the pull-tight region.
///
/// The eight extremes are upstream's `lx, ly, rx, uy, ulx, lrx, llx, urx`;
/// `to_int` floors the minima and ceils the maxima (`:113-125`).
#[derive(Debug, Clone)]
pub struct ChangedArea {
    layers: Vec<MutableOctagon>,
}

#[derive(Debug, Clone, Copy)]
struct MutableOctagon {
    lx: f64,
    ly: f64,
    rx: f64,
    uy: f64,
    ulx: f64,
    lrx: f64,
    llx: f64,
    urx: f64,
}

impl MutableOctagon {
    /// `MutableOctagon.set_empty` (`:104-113`), with Java's `Integer.MAX_VALUE`
    /// / `MIN_VALUE` sentinels.
    const EMPTY: MutableOctagon = MutableOctagon {
        lx: i32::MAX as f64,
        ly: i32::MAX as f64,
        rx: i32::MIN as f64,
        uy: i32::MIN as f64,
        ulx: i32::MAX as f64,
        lrx: i32::MIN as f64,
        llx: i32::MAX as f64,
        urx: i32::MIN as f64,
    };

    fn to_int(self) -> IntOctagon {
        if self.rx < self.lx || self.uy < self.ly || self.lrx < self.ulx || self.urx < self.llx {
            return IntOctagon::EMPTY;
        }
        IntOctagon::new(
            self.lx.floor() as i32,
            self.ly.floor() as i32,
            self.rx.ceil() as i32,
            self.uy.ceil() as i32,
            self.ulx.floor() as i32,
            self.lrx.ceil() as i32,
            self.llx.floor() as i32,
            self.urx.ceil() as i32,
        )
    }
}

impl ChangedArea {
    /// An empty changed area over `layer_count` layers.
    pub fn new(layer_count: usize) -> ChangedArea {
        ChangedArea {
            layers: vec![MutableOctagon::EMPTY; layer_count],
        }
    }

    /// `join(FloatPoint, layer)` (`:29-42`).
    pub fn join_point(&mut self, point: FloatPoint, layer: i32) {
        let Some(curr) = layer
            .try_into()
            .ok()
            .and_then(|l: usize| self.layers.get_mut(l))
        else {
            return;
        };
        curr.lx = curr.lx.min(point.x);
        curr.ly = curr.ly.min(point.y);
        curr.rx = curr.rx.max(point.x);
        curr.uy = curr.uy.max(point.y);
        let diff = point.x - point.y;
        curr.ulx = curr.ulx.min(diff);
        curr.lrx = curr.lrx.max(diff);
        let sum = point.x + point.y;
        curr.llx = curr.llx.min(sum);
        curr.urx = curr.urx.max(sum);
    }

    /// `join(TileShape, layer)` (`:47-56`): every corner of the shape.
    pub fn join_shape(&mut self, shape: &Simplex, layer: i32) {
        for index in 0..shape.border_line_count() {
            if shape.corner_is_bounded(index) {
                self.join_point(shape.corner_approx(index), layer);
            }
        }
    }

    /// `get_area(layer)` (`:61-64`): the smallest integer octagon containing
    /// the layer's marked region, empty when nothing was marked.
    pub fn area(&self, layer: i32) -> IntOctagon {
        match layer
            .try_into()
            .ok()
            .and_then(|l: usize| self.layers.get(l))
        {
            Some(octagon) => octagon.to_int(),
            None => IntOctagon::EMPTY,
        }
    }

    /// `set_empty(layer)` (`:87-89`).
    pub fn set_empty(&mut self, layer: i32) {
        if let Some(curr) = layer
            .try_into()
            .ok()
            .and_then(|l: usize| self.layers.get_mut(l))
        {
            *curr = MutableOctagon::EMPTY;
        }
    }

    /// True when no layer has been marked.
    pub fn is_empty(&self) -> bool {
        (0..self.layers.len()).all(|layer| self.area(layer as i32).is_empty())
    }

    /// Number of layers this area spans.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

/// The shove stack reports the geometry it inserted through this trait, which
/// is how `ShoveTraceAlgo.insert` reaches `board.join_changed_area` and
/// `board.changed_area.get_area(layer)` without `fr-board` owning the type.
impl ChangedAreaSink for ChangedArea {
    fn join_point(&mut self, point: FloatPoint, layer: i32) {
        ChangedArea::join_point(self, point, layer);
    }

    fn area(&self, layer: i32) -> Option<IntOctagon> {
        let area = ChangedArea::area(self, layer);
        (!area.is_empty()).then_some(area)
    }
}

/// Whether [`ForcedInserter::insert_pull_tight`] starts on.
///
/// **`true`, which is upstream** — the switch exists so the two sides can be
/// A/B'd on a fixture, not because either side is a divergence.
///
/// Upstream runs a full pull-tight over the *accumulated* trace after **every**
/// segment window of `InsertFoundConnectionAlgo.insert_trace`, keyed on that
/// window's end corner, and states the purpose at `RoutingBoard.java:868-869`:
/// "to avoid, that a separate handling for moving backwards in the own trace
/// line becomes necessary, pull tight is called here". The window that follows
/// is then checked against a straightened trace rather than against the
/// locator's raw staircase.
///
/// Measured at `-mp 1`, referee-judged, incomplete connections:
///
/// | fixture | mode | off | on |
/// |---|---|---|---|
/// | easy-10, all ten | default (optimizer on) | — | **identical, 0 added violations** |
/// | Issue034 | default, wall clock | 263.7 s | **97.4 s** |
/// | Issue026 | `--no-optimizer` | 5 | 8 |
/// | Issue155 | `--no-optimizer` | 5 | 5 |
/// | Issue159 | `--no-optimizer` | 2 | 3 |
/// | Issue102 | `--no-optimizer` | 43 | **41** (17.8 s vs 18.2 s) |
///
/// Two honest halves, and both are worth knowing:
///
/// * **the speed is real and structural.** Every later window, every later
///   shove and the batch driver's `opt_changed_area` all work on a trace with a
///   fraction of the corners, and corner count is what the clearance oracle is
///   priced in. That is the 2.7× on Issue034;
/// * **the router stage alone is not uniformly better.** Two of the easy-10 lose
///   connections when the optimizer is switched off (Issue026 5 → 8, Issue159
///   2 → 3) while Issue102 gains two. Straightening mid-insertion moves copper
///   the locator did not plan for, and this port's located chains are much
///   denser than the release's — 44 corners against the reference jar's 16 on
///   the first congested connection of Issue159, from a room decomposition that
///   crosses roughly three times as many doors along the same corridor. The
///   denser the chain, the further the straightening moves it. Closing that gap
///   is a rooms/maze job; when it lands, re-run the `--no-optimizer` rows above
///   and the spread should shrink.
///
/// The default stays upstream's because the *gate* is judged in default mode,
/// where the two are indistinguishable and this one is far faster.
pub const INSERT_PULL_TIGHT_DEFAULT: bool = true;

/// The board-mutating half of the connection inserter.
///
/// Owns the `&mut Board` for the whole insertion, so that the search-tree
/// queries the checks run against always see the copper inserted so far —
/// upstream's incremental behaviour, and the reason the per-segment ok-point
/// protocol works at all.
pub struct ForcedInserter<'b> {
    /// The board being mutated.
    pub board: &'b mut Board,
    /// The board rules (clearance matrix, net classes).
    pub rules: &'b BoardRules,
    /// The routed net's control.
    pub ctrl: &'b AutorouteControl,
    /// The region every mutation is joined into.
    pub changed_area: ChangedArea,
    /// Whether step 8 of `insert_forced_trace_polyline` — the in-insert
    /// pull-tight over the accumulated trace, with the segment's end corner as
    /// the keep point (`RoutingBoard.java:821-889`) — runs.
    ///
    /// Default [`INSERT_PULL_TIGHT_DEFAULT`], which is **`true`** — upstream's
    /// behaviour, not a divergence. The field exists so the two sides can be
    /// A/B'd on a fixture; that constant carries the measurements.
    pub insert_pull_tight: bool,
    bounds: IntBox,
    scratch_ids: Vec<ItemId>,
}

impl<'b> ForcedInserter<'b> {
    /// Wraps a board for one connection's insertion.
    pub fn new(
        board: &'b mut Board,
        rules: &'b BoardRules,
        ctrl: &'b AutorouteControl,
        bounds: IntBox,
    ) -> ForcedInserter<'b> {
        let layer_count = board.layers().len();
        ForcedInserter {
            board,
            rules,
            ctrl,
            changed_area: ChangedArea::new(layer_count),
            insert_pull_tight: INSERT_PULL_TIGHT_DEFAULT,
            bounds,
            scratch_ids: Vec::new(),
        }
    }

    /// Builder form of [`ForcedInserter::insert_pull_tight`].
    #[must_use]
    pub fn with_insert_pull_tight(mut self, on: bool) -> ForcedInserter<'b> {
        self.insert_pull_tight = on;
        self
    }

    /// The net array every item this inserter creates carries.
    fn nets(&self) -> NetNos {
        NetNos::of(self.ctrl.net_no)
    }

    // ---- the shove contexts ---------------------------------------------

    /// A read-only [`ShoveCheck`] over the same board.
    fn shove_check(&self) -> ShoveCheck<'_> {
        ShoveCheck::new(self.board, self.bounds)
    }

    /// A mutating [`ShoveInsert`] over the same board, reporting into this
    /// inserter's changed area.
    fn shove_insert(&mut self) -> ShoveInsert<'_> {
        let ForcedInserter {
            board,
            changed_area,
            bounds,
            ..
        } = self;
        ShoveInsert::new(board, *bounds, changed_area)
    }

    /// The three recursion budgets, straight from `AutorouteControl`.
    fn budgets(&self) -> (i32, i32, i32) {
        (
            self.ctrl.max_shove_trace_recursion_depth,
            self.ctrl.max_shove_via_recursion_depth,
            self.ctrl.max_spring_over_recursion_depth,
        )
    }

    // ---- the clearance oracle -------------------------------------------

    /// The instance `ShoveTraceAlgo.check` (`ShoveTraceAlgo.java:166-292`):
    /// can this trace shape go in, pushing whatever overlaps it aside?
    ///
    /// `from_side` is upstream's `CalcFromSide(combined_polyline, i + 1,
    /// shape)`; `p_dir` is `null` at the top level, because only the internal
    /// substitute recursion needs the anti-bounce-back filter.
    pub fn check_trace_shape_from(
        &mut self,
        shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
    ) -> bool {
        fr_metrics::counters::bump::shove_checks(1);
        let (max_rec, max_via_rec, max_spring_over_rec) = self.budgets();
        let nets = self.nets();
        let cl_class = self.ctrl.trace_clearance_class_no;
        let mut checker = self.shove_check();
        let ok = checker.check(
            shape,
            from_side,
            None,
            layer,
            &nets,
            cl_class,
            max_rec,
            max_via_rec,
            max_spring_over_rec,
        );
        if !ok && crate::maze::trace::is_enabled() {
            let blame = checker.failing_obstacle;
            let describe = |id: ItemId| {
                self.board.store().get(id).map(|item| {
                    let anchor = match &item.kind {
                        ItemKind::Pin(p) => {
                            format!("Pin@{},{}", p.center.x, p.center.y)
                        }
                        ItemKind::Via(v) => {
                            format!("Via@{},{}", v.center.x, v.center.y)
                        }
                        ItemKind::Trace(t) => {
                            let fc = t.polyline.first_corner();
                            let lc = t.polyline.last_corner();
                            format!(
                                "Trace@{},{}..{},{}",
                                fc.to_float().x as i32,
                                fc.to_float().y as i32,
                                lc.to_float().x as i32,
                                lc.to_float().y as i32
                            )
                        }
                        _ => "other".to_string(),
                    };
                    let slot = self.board.store().slot_of(id);
                    let tree_bb = slot
                        .map(|s| self.board.trees().default_tree().item_shapes(s, id))
                        .and_then(|tiles| {
                            tiles
                                .iter()
                                .find(|t| t.layer == layer)
                                .map(|t| t.shape.as_ref().map(|s| s.bounding_box()))
                        })
                        .flatten();
                    let tree_bb = tree_bb
                        .map(|b| format!("tree=[({},{})..({},{})]", b.ll.x, b.ll.y, b.ur.x, b.ur.y))
                        .unwrap_or_else(|| "tree=none".into());
                    format!(
                        "{:?} {} {} nets={:?} fixed={:?} slot={:?}",
                        id, anchor, tree_bb, item.nets, item.fixed_state, slot
                    )
                })
            };
            // What did the tree query return for this exact shape?
            let mut queried = Vec::new();
            self.board.overlapping_items(
                &TileShape::Simplex(shape.clone()),
                Some(layer),
                &[],
                &mut queried,
            );
            queried.sort_unstable();
            let qlist = queried
                .iter()
                .filter_map(|id| describe(*id))
                .collect::<Vec<_>>()
                .join(" | ");
            let bb = shape.bounding_box();
            crate::maze::trace::conn_landing(&format!(
                "FRCONN check_fail net={} layer={} shape=[({},{})..({},{})] blame={}",
                self.ctrl.net_no,
                layer,
                bb.ll.x,
                bb.ll.y,
                bb.ur.x,
                bb.ur.y,
                blame.and_then(describe).unwrap_or_else(|| "outline".into()),
            ));
            crate::maze::trace::conn_landing(&format!(
                "FRCONN check_fail_query net={} n={} items: {}",
                self.ctrl.net_no,
                queried.len(),
                qlist,
            ));
        }
        ok
    }

    /// [`Self::check_trace_shape_from`] with no from-side, which makes
    /// `ShapeTraceEntries` derive one from its own-net entries
    /// (`ShapeTraceEntries.search_from_side`, `:372-388`).
    pub fn check_trace_shape(&mut self, shape: &Simplex, layer: i32) -> bool {
        self.check_trace_shape_from(shape, None, layer)
    }

    /// `ShoveTraceAlgo.insert` (`:297-418`), as a transaction: on failure the
    /// board is restored exactly, so the caller's rollback contract holds
    /// without the caller owning an undo.
    pub fn insert_trace_shape(
        &mut self,
        shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
    ) -> bool {
        let (max_rec, max_via_rec, max_spring_over_rec) = self.budgets();
        let nets = self.nets();
        let cl_class = self.ctrl.trace_clearance_class_no;
        let mut inserter = self.shove_insert();
        inserter.insert_transaction(
            shape,
            from_side,
            layer,
            &nets,
            cl_class,
            &[],
            max_rec,
            max_via_rec,
            max_spring_over_rec,
        )
    }

    /// `ForcedPadAlgo.check_forced_pad` (`ForcedPadAlgo.java:141-226`).
    ///
    /// One approximation is inherited from M2a and unchanged: the *tree* the
    /// obstacle query runs against is chosen by
    /// [`fr_board::shove::geom::overlapping_items_with_clearance`], which uses
    /// the default (uncompensated) tree exactly as upstream's shove stack does
    /// — `SearchTreeManager.clearance_compensation_used` is `false` in every
    /// headless run (`SearchTreeManager.java:34`; only the GUI flips it).
    pub fn check_forced_pad(
        &mut self,
        shape: &Simplex,
        layer: i32,
        attach_smd_allowed: bool,
    ) -> bool {
        self.check_forced_pad_with(
            shape,
            None,
            layer,
            attach_smd_allowed,
            self.ctrl.trace_clearance_class_no,
        ) != CheckDrillResult::NotDrillable
    }

    /// [`Self::check_forced_pad`] with the pad's own from-side and clearance
    /// class, which is what the forced-via path needs.
    pub fn check_forced_pad_with(
        &mut self,
        shape: &Simplex,
        from_side: Option<FromSide>,
        layer: i32,
        attach_smd_allowed: bool,
        cl_class: i32,
    ) -> CheckDrillResult {
        let (max_rec, max_via_rec, _) = self.budgets();
        let nets = self.nets();
        let mut checker = self.shove_check();
        let pad_tile = fr_board::TileShape::Simplex(shape.clone());
        checker.check_forced_pad(
            &pad_tile,
            from_side,
            layer,
            &nets,
            cl_class,
            attach_smd_allowed,
            &[],
            max_rec,
            max_via_rec,
            false,
        )
    }

    /// `RoutingBoard.check_trace_segment` (`:250-306`): the length along
    /// `from → to` that fits without conflict, or [`f64::INFINITY`] when the
    /// whole segment does (upstream's `Integer.MAX_VALUE`).
    ///
    /// `only_not_shovable` skips routable, non-shove-fixed obstacles — which is
    /// what the neckdown caller passes, because those are the ones shove would
    /// have handled. With shove off that makes the neckdown *more* eager than
    /// upstream, never less: it necks down only where a hard obstacle is close.
    pub fn check_trace_segment(
        &mut self,
        from: IntPoint,
        to: IntPoint,
        layer: i32,
        half_width: i32,
        only_not_shovable: bool,
    ) -> f64 {
        if from == to {
            return 0.0;
        }
        let polyline = Polyline::from_int_points(&[from, to]);
        if polyline.arr.len() != 3 {
            return 0.0;
        }
        let Some(shape_to_check) = polyline.offset_shape(half_width, 0) else {
            return 0.0;
        };
        let from_point = from.to_float();
        let to_point = to.to_float();
        let line_length = to_point.distance(&from_point);
        if line_length == 0.0 {
            return 0.0;
        }
        let mut ok_length = f64::INFINITY;

        let net_no = self.ctrl.net_no;
        let cl_class = self.ctrl.trace_clearance_class_no;
        let ForcedInserter { board, .. } = self;
        let mut obstacles = Vec::new();
        fr_board::shove::geom::overlapping_items_with_clearance(
            board,
            &shape_to_check,
            layer,
            &[net_no],
            cl_class,
            &mut obstacles,
        );
        let tree = board.default_tree();

        for id in obstacles {
            let Some(item) = board.store().get(id) else {
                continue;
            };
            if only_not_shovable && item.is_routable() && !item.is_shove_fixed() {
                continue;
            }
            let Some(slot) = board.store().slot_of(id) else {
                continue;
            };
            let clearance_value = board
                .clearances()
                .value(item.clearance_class, cl_class, layer);
            let curr_offset_shape = shape_to_check.offset(clearance_value as f64);
            let shorten_value = half_width as f64 + clearance_value as f64;
            for tile in tree.item_shapes(slot, id) {
                if tile.layer != layer {
                    continue;
                }
                let Some(obstacle_shape) = tile.shape.as_ref().map(crate::geom::tile_to_simplex)
                else {
                    continue;
                };
                let intersection = obstacle_shape.intersection(&curr_offset_shape);
                if intersection.is_empty() {
                    continue;
                }
                let nearest = intersection.nearest_point_approx(&from_point);
                let projection = from_point.scalar_product(&to_point, &nearest) / line_length;
                let projection = (projection - shorten_value - 1.0).max(0.0);
                if projection < ok_length {
                    ok_length = projection;
                    if ok_length <= 0.0 {
                        return 0.0;
                    }
                }
            }
        }
        ok_length
    }

    // ---- the shove core, shove-free (§4.6) -------------------------------

    /// `RoutingBoard.insert_forced_trace_polyline` (`:591-899`).
    ///
    /// Returns the last corner insertion reached: `to_corner` on full success,
    /// an intermediate or the first corner on partial success, `None` when the
    /// board may be damaged. The caller branches on all three — that is the
    /// ok-point trichotomy of spec §11 ("Keep semantically").
    ///
    /// M2a can only return `None` for a degenerate polyline: nothing is shoved,
    /// so `shove.insert` — upstream's only other `null` — cannot fail.
    pub fn insert_forced_trace_polyline(
        &mut self,
        polyline: &Polyline,
        half_width: i32,
        layer: i32,
        tidy_width: i32,
    ) -> Option<Point> {
        let (Some(from_corner), Some(to_corner)) = (
            int_corner(&polyline.first_corner()),
            int_corner(&polyline.last_corner()),
        ) else {
            // `:597-609`: a degenerate polyline has no well-defined end
            // corners. Upstream returns null so the caller reroutes.
            return None;
        };
        if from_corner == to_corner {
            return Some(Point::Int(to_corner));
        }

        // `:614-632` — an item of the same net ending at `from_corner` lends
        // its geometry so the junction check shape has no dog ears.
        let picked_polyline = self.pick_combinable_trace(from_corner, layer, half_width);

        // `RoutingBoard.java:632`: `p_half_width +
        // search_tree.clearance_compensation_value(cl, layer)` on the **default**
        // tree, whose compensation is off in every headless run
        // (`SearchTreeManager.java:34`) — so this is plain `half_width` there,
        // and the clearance is added by the shove's own
        // `overlapping_items_with_clearance`. Adding the compensation here as
        // well would inflate every check shape by an extra half clearance.
        let compensated_half_width = fr_board::shove::entries::compensated_half_width(
            self.board,
            half_width,
            self.ctrl.trace_clearance_class_no,
            layer,
        );
        let orthogonal_mode = self.board.angle_restriction() == AngleRestriction::Ninety;

        // Step 3, `:641-643`: `spring_over_obstacles` on the *whole* polyline,
        // with its own `c_max_spring_over_recursion_depth = 20`. `null` means
        // "nothing inserted, board clean", which the caller reads as a
        // first-corner ok-point.
        let mut new_polyline = {
            let nets = self.nets();
            let cl_class = self.ctrl.trace_clearance_class_no;
            let mut checker = self.shove_check();
            match checker.spring_over_obstacles(
                polyline,
                compensated_half_width,
                layer,
                &nets,
                cl_class,
            ) {
                Some(sprung) => sprung,
                None => {
                    if crate::maze::trace::is_enabled() {
                        crate::maze::trace::conn_landing(&format!(
                            "FRCONN insert_polyline_fail net={} step=spring_over_none from={:?} to={:?}",
                            self.ctrl.net_no, from_corner, to_corner
                        ));
                    }
                    return Some(Point::Int(from_corner));
                }
            }
        };

        let mut combined_polyline = match picked_polyline.as_ref() {
            Some(picked) => new_polyline.combine(picked),
            None => new_polyline.clone(),
        };
        if combined_polyline.arr.len() < 3 {
            return Some(Point::Int(from_corner));
        }

        let start_shape_no = combined_polyline
            .arr
            .len()
            .saturating_sub(new_polyline.arr.len());
        let trace_shapes = combined_polyline.offset_shapes_range(
            compensated_half_width,
            start_shape_no,
            combined_polyline.arr.len() - 1,
        );

        // Step 4, `:673-707`: check *and* shove-insert every shape, in order.
        // A check failure stops the walk and hands the rest to the resampler;
        // an insert failure is upstream's `null`.
        let mut last_shape_no = trace_shapes.len();
        let corner_count = combined_polyline.corner_count();
        for (index, shape) in trace_shapes.iter().enumerate() {
            let shape = orthogonalize(shape, orthogonal_mode);
            let from_side_index = (corner_count + index).saturating_sub(trace_shapes.len() + 1);
            let from_side = FromSide::from_polyline(&combined_polyline, from_side_index, &shape);
            if !self.check_trace_shape_from(&shape, Some(from_side), layer) {
                last_shape_no = index;
                break;
            }
            if !self.insert_trace_shape(&shape, Some(from_side), layer) {
                return None;
            }
        }

        let mut new_corner = Point::Int(to_corner);

        // Step 5, `:715-800`: resample the failing segment and retry once.
        if last_shape_no < trace_shapes.len() {
            let sample_width = 2 * self.board.min_trace_half_width();
            let last = new_polyline.corner_approx(last_shape_no + 1);
            let prev_last = new_polyline.corner_approx(last_shape_no);
            let last_segment_length = last.distance(&prev_last);
            if last_segment_length > 100.0 * sample_width as f64 {
                // Too many cycles to sample (`:725`).
                return Some(Point::Int(from_corner));
            }
            let mut last_trace_shape = orthogonalize(&trace_shapes[last_shape_no], orthogonal_mode);
            let mut shape_index = (combined_polyline.corner_count() + last_shape_no)
                .saturating_sub(trace_shapes.len() + 1);
            if last_segment_length > sample_width as f64 {
                new_polyline = new_polyline.shorten(
                    new_polyline
                        .arr
                        .len()
                        .saturating_sub(trace_shapes.len() - last_shape_no - 1),
                    sample_width as f64,
                );
                let Some(curr_last_corner) = int_corner(&new_polyline.last_corner()) else {
                    return Some(Point::Int(from_corner));
                };
                new_corner = Point::Int(curr_last_corner);
                combined_polyline = match picked_polyline.as_ref() {
                    Some(picked) => new_polyline.combine(picked),
                    None => new_polyline.clone(),
                };
                if combined_polyline.arr.len() < 3 {
                    return Some(new_corner);
                }
                shape_index = combined_polyline.arr.len() - 3;
                let Some(shape) =
                    combined_polyline.offset_shape(compensated_half_width, shape_index)
                else {
                    return Some(Point::Int(from_corner));
                };
                last_trace_shape = orthogonalize(&shape, orthogonal_mode);
            }
            let from_side =
                FromSide::from_polyline(&combined_polyline, shape_index, &last_trace_shape);
            if !self.check_trace_shape_from(&last_trace_shape, Some(from_side), layer) {
                if crate::maze::trace::is_enabled() {
                    crate::maze::trace::conn_landing(&format!(
                        "FRCONN insert_polyline_fail net={} step=step5_check_failed from={:?} to={:?} last_shape_no={}/{}",
                        self.ctrl.net_no, from_corner, to_corner, last_shape_no, trace_shapes.len()
                    ));
                }
                return Some(Point::Int(from_corner));
            }
            if !self.insert_trace_shape(&last_trace_shape, Some(from_side), layer) {
                return None;
            }
        }

        // Step 6, `:801-807`: mark and insert.
        for index in 0..new_polyline.corner_count() {
            self.changed_area
                .join_point(new_polyline.corner_approx(index), layer);
        }
        let nets = self.nets();
        let Some(trace_id) = self.board.insert_trace_without_cleaning(
            new_polyline,
            layer,
            half_width,
            nets,
            self.ctrl.trace_clearance_class_no,
            FixedState::Unfixed,
        ) else {
            // A zero-length or single-corner polyline: nothing inserted, and
            // nothing to report as reached beyond where we started.
            return Some(Point::Int(from_corner));
        };
        self.board.combine_trace(trace_id);

        // Step 7, `:834-855`: normalize against the changed-area clip, so
        // generated cycles are gone before pull-tight runs — upstream's own
        // comment at `:837-838` is "remove evtl. generated cycles because
        // otherwise pull_tight may not work correctly".
        let clip = self.changed_area.area(layer);
        let normalized = self.board.normalize_trace(trace_id, Some(&clip));

        // Step 8, `:821-889`: the in-insert pull-tight, with `new_corner` as
        // the keep point. Gated on [`ForcedInserter::insert_pull_tight`], whose
        // default is [`INSERT_PULL_TIGHT_DEFAULT`] — **on**, which is upstream's
        // behaviour; the field's docs carry the A/B measurement.
        if !self.insert_pull_tight {
            return Some(new_corner);
        }
        let tidy_region = if tidy_width < i32::MAX {
            Some(
                new_corner
                    .surrounding_octagon()
                    .offset(f64::from(tidy_width)),
            )
        } else {
            None
        };
        // `:825-831`: the net filter is the routed net **only** when the shove
        // is switched off; with a shove depth the region's other nets relax too.
        let opt_nets: Vec<i32> = if self.ctrl.max_shove_trace_recursion_depth <= 0 {
            self.nets().iter().copied().collect()
        } else {
            Vec::new()
        };
        let accuracy = self.ctrl.pull_tight_accuracy;
        // `trace_pull_tight` records what it moved; the inserter's region is the
        // one the caller's `opt_changed_area` reads, so it is lent, not shadowed.
        let mut changed_area = std::mem::replace(&mut self.changed_area, ChangedArea::new(0));
        let mut trace = Some(trace_id);
        {
            // The stall guard, and why it is the round-counted one in *both*
            // budget modes.
            //
            // Upstream arms none at all here: `:834` passes `p_time_limit = -1`,
            // which `PullTightAlgo`'s constructor reads as "no `TimeLimit`"
            // (`:56-60`). Copying the `-1` verbatim would leave this loop —
            // which runs once per *segment* of every forced insertion, not once
            // per connection — with nothing but
            // [`crate::pull_tight::DEFAULT_ROUND_BUDGET`]'s two million rounds
            // behind it. But a wall cutoff is the wrong bound here in a way it
            // is not at the batch driver's regional call: this pull-tight moves
            // copper the rest of the insertion then builds on, so a deadline
            // that fires on a loaded machine changes what gets *inserted*, not
            // just how tidy it is.
            //
            // So the guard is `PULL_TIGHT_TIME_LIMIT` — the budget upstream
            // gives its other non-batch pull-tight (`RoutingBoard.java:49`,
            // `:144`) — spent in optimization rounds at
            // [`crate::pull_tight::ROUNDS_PER_VIRTUAL_MS`]. That is ~12× the
            // heaviest whole-connection regional pull-tight measured in the
            // fixture suite, so it is a safety valve and not a knob, and it
            // reads the clock never. The job deadline is still polled
            // underneath it, which is what makes a `SIGTERM` mid-insertion
            // unwind instead of hang.
            let mut algo = crate::pull_tight::PullTightAlgo::new(
                self.board,
                self.rules,
                &opt_nets,
                tidy_region,
                accuracy,
                None,
                Some(new_corner.clone()),
                layer,
            )
            .with_stall_guard(crate::pull_tight::StallGuard::for_mode(
                BudgetMode::Deterministic,
                crate::pull_tight::PULL_TIGHT_TIME_LIMIT_MS,
            ));
            if normalized {
                // `:849-871`: the keep point must stay a corner of the net, and
                // the `new_trace` handle is stale after the split — upstream
                // drops it and re-picks at `new_corner`.
                algo.split_traces_at_keep_point();
                trace = algo.picked_trace_at(&new_corner, layer);
            }
            // `:878-880`: `p_tidy_width > 0 && new_trace != null`.
            if tidy_width > 0 {
                if let Some(id) = trace {
                    algo.trace_pull_tight(id, &mut changed_area);
                }
            }
        }
        self.changed_area = changed_area;

        Some(new_corner)
    }

    /// `RoutingBoard.insert_forced_trace_segment` (`:533-553`) — the
    /// two-corner overload.
    ///
    /// Two things it does that the polyline entry point does not, both
    /// load-bearing for the neckdown splitter that calls it:
    ///
    /// * a zero-length segment succeeds immediately (`:537-539`). `try_neck_down`
    ///   routinely asks for one, because the rounding knee it inserts can land
    ///   exactly on the neckdown end point;
    /// * the ok-point is mapped back onto the *caller's* corners (`:545-551`),
    ///   which upstream needs because its callers compare by object identity.
    ///   Here it is a no-op on the value, and kept so the transcription reads
    ///   the same.
    pub fn insert_forced_trace_segment(
        &mut self,
        from: IntPoint,
        to: IntPoint,
        half_width: i32,
        layer: i32,
    ) -> Option<Point> {
        if from == to {
            return Some(Point::Int(to));
        }
        let polyline = Polyline::from_int_points(&[from, to]);
        let first = polyline.first_corner();
        let last = polyline.last_corner();
        let ok_point = self.insert_forced_trace_polyline(&polyline, half_width, layer, i32::MAX)?;
        Some(if ok_point == first {
            Point::Int(from)
        } else if ok_point == last {
            Point::Int(to)
        } else {
            ok_point
        })
    }

    /// `:614-632`: the single same-net trace ending at `corner` whose width and
    /// clearance class match, or `None`.
    fn pick_combinable_trace(
        &mut self,
        corner: IntPoint,
        layer: i32,
        half_width: i32,
    ) -> Option<Polyline> {
        let mut picked = std::mem::take(&mut self.scratch_ids);
        picked.clear();
        self.board
            .pick_items(&Point::Int(corner), Some(layer), &mut picked);
        picked.retain(|id| {
            self.board
                .store()
                .get(*id)
                .is_some_and(|item| matches!(item.kind, ItemKind::Trace(_)))
        });
        let result = if picked.len() == 1 {
            self.board.store().get(picked[0]).and_then(|item| {
                let ItemKind::Trace(trace) = &item.kind else {
                    return None;
                };
                let matches = item.contains_net(self.ctrl.net_no)
                    && item.net_count() == 1
                    && trace.half_width == half_width
                    && item.clearance_class == self.ctrl.trace_clearance_class_no;
                matches.then(|| trace.polyline.clone())
            })
        } else {
            None
        };
        picked.clear();
        self.scratch_ids = picked;
        result
    }

    // ---- forced vias (§4.4) ---------------------------------------------

    /// `ForcedViaAlgo.check` (`:89-152`) at shove depth 0, followed by
    /// `ForcedViaAlgo.insert` (`:165-232`) on success.
    ///
    /// Returns whether the via went in. Upstream separates the two so that
    /// `insert_via` can pick the cheapest *checkable* mask before committing;
    /// [`Self::check_via`] is that half.
    pub fn insert_forced_via(&mut self, via_info: &ViaInfo, location: IntPoint) -> bool {
        let Some(padstack_no) = self.resolve_via_padstack(via_info) else {
            return false;
        };
        let padstack = via_info.padstack();
        let from_layer = padstack.from_layer() as i32;
        let to_layer = padstack.to_layer() as i32;
        let (pads, hole_shape) = self.via_layer_pads(via_info, location);
        let (max_rec, max_via_rec, _) = self.budgets();
        let nets = self.nets();
        let attach = via_info.attach_smd_allowed();
        let cl_class = self.ctrl.trace_clearance_class_no;

        // `ForcedViaAlgo.insert` (`:165-232`), as a transaction: on failure the
        // partially shoved board is restored, which is what upstream needs an
        // external undo for.
        let mark = self.board.begin();
        let inserted = {
            let mut inserter = self.shove_insert();
            inserter
                .forced_via_pads(
                    &pads,
                    location,
                    &nets,
                    attach,
                    hole_shape.as_ref(),
                    cl_class,
                    max_rec,
                    max_via_rec,
                )
                .is_ok()
        };
        if !inserted {
            self.board.rollback_to(mark);
            return false;
        }

        // DRC symmetric acceptance check on each layer: ensure the placed via has
        // no clearance violations against the live board state.
        for pad in &pads {
            if !self.check_drc_clear(&pad.shape, pad.layer, &nets, pad.clearance_class) {
                self.board.rollback_to(mark);
                return false;
            }
        }
        self.board.commit(mark);

        for layer in from_layer..=to_layer {
            self.changed_area.join_point(location.to_float(), layer);
        }
        let nets = self.nets();
        self.board.insert_via(
            Via {
                center: location,
                padstack_no,
                layers: LayerRange::new(from_layer, to_layer),
                attach_allowed: via_info.attach_smd_allowed(),
                is_escape_via: false,
                escape_via_smd_layer: -1,
            },
            nets,
            via_info.clearance_class() as i32,
            FixedState::Unfixed,
        );
        true
    }

    /// `ForcedViaAlgo.check` (`:89-158`) — every padstack layer, the
    /// hole-clearance probe of spec §13.2, and the start-trace circle of
    /// conformance delta **B5**.
    pub fn check_via(&mut self, via_info: &ViaInfo, location: IntPoint) -> bool {
        let (pads, hole_shape) = self.via_layer_pads(via_info, location);
        let (max_rec, max_via_rec, _) = self.budgets();
        let nets = self.nets();
        let attach = via_info.attach_smd_allowed();
        let cl_class = self.ctrl.trace_clearance_class_no;
        let mut checker = self.shove_check();
        if checker
            .check_forced_via(
                &pads,
                location,
                &nets,
                attach,
                hole_shape.as_ref(),
                cl_class,
                max_rec,
                max_via_rec,
            )
            .is_err()
        {
            return false;
        }
        if max_rec <= 0 {
            for pad in &pads {
                if !self.check_drc_clear(&pad.shape, pad.layer, &nets, pad.clearance_class) {
                    return false;
                }
            }
        }
        true
    }

    /// DRC symmetric clearance acceptance check: verifies that `shape` has zero
    /// clearance violations against the board's uncompensated default tree, using
    /// exact DRC arithmetic (both shapes enlarged by `clearance / 2`, no safety margin).
    pub fn check_drc_clear(
        &self,
        shape: &Simplex,
        layer: i32,
        net_nos: &NetNos,
        cl_class: i32,
    ) -> bool {
        let max_cl = fr_board::shove::geom::max_clearance_of(self.board, cl_class, layer);
        let reach = 1.5 * max_cl as f64;
        let broad = TileShape::Simplex(shape.offset(reach));
        let mut candidates = Vec::new();
        self.board
            .overlapping_items(&broad, Some(layer), net_nos.as_slice(), &mut candidates);
        if candidates.is_empty() {
            return true;
        }

        let tree = self.board.default_tree();
        let mut curr_half_cl = -1i32;
        let mut curr_offset_simplex = shape.clone();

        for id in candidates {
            let Some(item) = self.board.store().get(id) else {
                continue;
            };
            if item.nets.shares_any(net_nos) {
                continue;
            }
            if let ItemKind::ConductionArea(a) = &item.kind {
                if !a.is_obstacle {
                    continue;
                }
            }
            if let ItemKind::ObstacleArea(a) = &item.kind {
                if a.kind == fr_board::item::ObstacleKind::ComponentKeepout
                    || a.kind == fr_board::item::ObstacleKind::ViaKeepout
                {
                    continue;
                }
            }
            let cl = self
                .board
                .clearances()
                .value(cl_class, item.clearance_class, layer);
            let half = cl / 2;
            if half != curr_half_cl {
                curr_half_cl = half;
                curr_offset_simplex = shape.enlarge(half as f64);
            }
            let Some(slot) = self.board.store().slot_of(id) else {
                continue;
            };
            for tile in tree.item_shapes(slot, id) {
                if tile.layer != layer {
                    continue;
                }
                let Some(tile_shape) = tile.shape.as_ref() else {
                    continue;
                };
                let tile_simplex =
                    fr_board::shove::geom::tile_to_simplex(&tile_shape.enlarge(half));
                if curr_offset_simplex.intersection(&tile_simplex).dimension() == 2 {
                    return false;
                }
            }
        }
        true
    }

    /// Verifies that all shapes of item `id` have zero clearance violations
    /// against the board's uncompensated default tree, using exact DRC arithmetic.
    pub fn check_drc_clear_for_item(&self, id: ItemId) -> bool {
        let Some(item) = self.board.store().get(id) else {
            return true;
        };
        let Some(slot) = self.board.store().slot_of(id) else {
            return true;
        };
        let tree = self.board.default_tree();
        let shapes = tree.item_shapes(slot, id).to_vec();
        for tile in shapes {
            let Some(tile_shape) = tile.shape.as_ref() else {
                continue;
            };
            let simplex = fr_board::shove::geom::tile_to_simplex(tile_shape);
            let max_cl = fr_board::shove::geom::max_clearance_of(
                self.board,
                item.clearance_class,
                tile.layer,
            );
            let reach = 1.5 * max_cl as f64;
            let broad = TileShape::Simplex(simplex.offset(reach));
            let mut candidates = Vec::new();
            self.board
                .overlapping_items(&broad, Some(tile.layer), &[], &mut candidates);

            let mut curr_half_cl = -1i32;
            let mut curr_offset_simplex = simplex.clone();

            for other_id in candidates {
                if other_id == id {
                    continue;
                }
                let Some(other_item) = self.board.store().get(other_id) else {
                    continue;
                };
                if !other_item.is_obstacle(item) {
                    continue;
                }
                let cl = self.board.clearances().value(
                    item.clearance_class,
                    other_item.clearance_class,
                    tile.layer,
                );
                let half = cl / 2;
                if half != curr_half_cl {
                    curr_half_cl = half;
                    curr_offset_simplex = simplex.enlarge(half as f64);
                }
                let Some(other_slot) = self.board.store().slot_of(other_id) else {
                    continue;
                };
                for other_tile in tree.item_shapes(other_slot, other_id) {
                    if other_tile.layer != tile.layer {
                        continue;
                    }
                    let Some(other_shape) = other_tile.shape.as_ref() else {
                        continue;
                    };
                    let other_simplex =
                        fr_board::shove::geom::tile_to_simplex(&other_shape.enlarge(half));
                    if curr_offset_simplex.intersection(&other_simplex).dimension() == 2 {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// The per-layer pad shapes of `via_info` translated to `location`, plus
    /// the hole-clearance substitute shape (`ForcedViaAlgo.hole_check_shape`,
    /// `:240-252`).
    ///
    /// A copper-less padstack layer contributes the hole shape at clearance
    /// class **0** instead of being skipped, which is the §13.2 behaviour the
    /// release ships.
    fn via_layer_pads(
        &self,
        via_info: &ViaInfo,
        location: IntPoint,
    ) -> (Vec<ViaLayerPad>, Option<Simplex>) {
        let padstack = via_info.padstack();
        let orthogonal = self.board.angle_restriction() == AngleRestriction::Ninety;
        let hole_shape = fr_board::shove::via::hole_check_shape(
            location,
            padstack.get_drill_radius(),
            self.rules.get_hole_clearance(),
            orthogonal,
        );
        let mut pads = Vec::new();
        for layer in padstack.from_layer()..=padstack.to_layer() {
            let (shape, clearance_class) = match padstack.get_shape(layer) {
                Some(pad) => (
                    pad_tile_shape(pad, location, orthogonal),
                    via_info.clearance_class() as i32,
                ),
                None => match hole_shape.as_ref() {
                    Some(hole) => (hole.clone(), 0),
                    None => continue,
                },
            };
            pads.push(ViaLayerPad {
                layer: layer as i32,
                shape,
                clearance_class,
                trace_pen_half_width: self
                    .ctrl
                    .trace_half_width
                    .get(layer)
                    .copied()
                    .unwrap_or_default(),
            });
        }
        (pads, hole_shape)
    }

    /// The board padstack index carrying this via's geometry, appended to the
    /// table on first use.
    ///
    /// The board's padstack table only holds geometry some item already used;
    /// a via rule that no board item has instantiated yet needs its entry
    /// created. Mirrors `fr_io::specctra::ses_common::resolve_or_insert`.
    fn resolve_via_padstack(&mut self, via_info: &ViaInfo) -> Option<u32> {
        let identity = board_padstack_of(via_info.padstack())?;
        let existing = (0..self.board.padstacks().len()).find(|index| {
            self.board
                .padstacks()
                .get(*index as u32)
                .is_some_and(|candidate| *candidate == identity)
        });
        Some(match existing {
            Some(index) => index as u32,
            None => self.board.padstacks_mut().push(identity),
        })
    }

    // ---- tails and trace joining (§4.5) ----------------------------------

    /// `BasicBoard.get_trace_tail(location, layer, nets)` (`:1857-1883`): a
    /// same-net trace ending at `location` with **no** contact at that end.
    pub fn get_trace_tail(&mut self, location: IntPoint, layer: i32) -> Option<ItemId> {
        let point = Point::Int(location);
        let mut picked = std::mem::take(&mut self.scratch_ids);
        picked.clear();
        self.board.pick_items(&point, Some(layer), &mut picked);
        let mut contacts = Vec::new();
        let mut result = None;
        for id in picked.iter().copied() {
            let Some(item) = self.board.store().get(id) else {
                continue;
            };
            let ItemKind::Trace(trace) = &item.kind else {
                continue;
            };
            if !item.contains_net(self.ctrl.net_no) || item.net_count() != 1 {
                continue;
            }
            let ends_here = trace.first_corner() == point || trace.last_corner() == point;
            if !ends_here {
                continue;
            }
            self.board.normal_contacts_at(id, &point, &mut contacts);
            if contacts.is_empty() {
                result = Some(id);
                break;
            }
        }
        picked.clear();
        self.scratch_ids = picked;
        result
    }

    /// `RoutingBoard.connect_to_trace` (`:1089-1141`, spec §4.5): drop the
    /// perpendicular from `from_point` onto `to_trace`, then clean up the tails
    /// the split may have left at the original trace's two end corners.
    pub fn connect_to_trace(
        &mut self,
        from_point: IntPoint,
        to_trace: ItemId,
        half_width: i32,
    ) -> bool {
        let Some((polyline, layer, first_corner, last_corner)) =
            self.board.store().get(to_trace).and_then(|item| {
                let ItemKind::Trace(trace) = &item.kind else {
                    return None;
                };
                Some((
                    trace.polyline.clone(),
                    trace.layer,
                    trace.first_corner(),
                    trace.last_corner(),
                ))
            })
        else {
            return false;
        };
        let point = Point::Int(from_point);
        if polyline.contains(&point) {
            // No connection line necessary.
            return true;
        }
        let Some(projection_line) = polyline.projection_line(&point) else {
            return false;
        };
        let connection_line = projection_line.to_polyline();
        if connection_line.arr.len() != 3 {
            return false;
        }
        if !self.check_polyline_trace(&connection_line, layer, half_width) {
            return false;
        }
        for index in 0..connection_line.corner_count() {
            self.changed_area
                .join_point(connection_line.corner_approx(index), layer);
        }
        let nets = self.nets();
        let clip = self.changed_area.area(layer);
        self.board.insert_trace(
            connection_line,
            layer,
            half_width,
            nets,
            self.ctrl.trace_clearance_class_no,
            FixedState::Unfixed,
            Some(&clip),
        );

        for corner in [first_corner, last_corner] {
            if corner == point {
                continue;
            }
            let Point::Int(corner) = corner else {
                continue;
            };
            if let Some(tail) = self.get_trace_tail(corner, layer) {
                let user_fixed = self
                    .board
                    .store()
                    .get(tail)
                    .is_some_and(|item| item.fixed_state >= FixedState::UserFixed);
                if !user_fixed {
                    self.board.remove_item(tail);
                }
            }
        }
        true
    }

    /// `BasicBoard.check_polyline_trace` (`:1229-1245`) with `contact_pins`
    /// left null — see [`Self::check_trace_shape`].
    pub fn check_polyline_trace(
        &mut self,
        polyline: &Polyline,
        layer: i32,
        half_width: i32,
    ) -> bool {
        // `RoutingBoard.java:632`: `p_half_width +
        // search_tree.clearance_compensation_value(cl, layer)` on the **default**
        // tree, whose compensation is off in every headless run
        // (`SearchTreeManager.java:34`) — so this is plain `half_width` there,
        // and the clearance is added by the shove's own
        // `overlapping_items_with_clearance`. Adding the compensation here as
        // well would inflate every check shape by an extra half clearance.
        let compensated_half_width = fr_board::shove::entries::compensated_half_width(
            self.board,
            half_width,
            self.ctrl.trace_clearance_class_no,
            layer,
        );
        let orthogonal = self.board.angle_restriction() == AngleRestriction::Ninety;
        for shape in polyline.offset_shapes(compensated_half_width) {
            let shape = orthogonalize(&shape, orthogonal);
            if !self.check_trace_shape(&shape, layer) {
                return false;
            }
        }
        true
    }
}

/// `if (orthogonal_mode) curr_trace_shape = curr_trace_shape.bounding_box()`
/// (`RoutingBoard.java:676-678`).
fn orthogonalize(shape: &Simplex, orthogonal_mode: bool) -> Simplex {
    if orthogonal_mode && shape.is_bounded() {
        shape.bounding_box().to_simplex()
    } else {
        shape.clone()
    }
}

/// The integer corner of a polyline end, or `None` when it is not one —
/// upstream's `instanceof IntPoint` guard (`RoutingBoard.java:610-613`).
fn int_corner(point: &Point) -> Option<IntPoint> {
    match point {
        Point::Int(p) => Some(*p),
        Point::Rational(_) => None,
    }
}

/// One pad of a via padstack, placed at `location` and lowered to the tile
/// flavor the board's angle mode uses (`ForcedViaAlgo.java:108-114`).
fn pad_tile_shape(pad: &PadShape, location: IntPoint, orthogonal: bool) -> Simplex {
    match pad {
        PadShape::Box(b) => IntBox::new(
            b.left_x() + location.x,
            b.bottom_y() + location.y,
            b.right_x() + location.x,
            b.top_y() + location.y,
        )
        .to_simplex(),
        PadShape::Octagon(o) => {
            if orthogonal {
                let bb = o.bounding_box();
                IntBox::new(
                    bb.left_x() + location.x,
                    bb.bottom_y() + location.y,
                    bb.right_x() + location.x,
                    bb.top_y() + location.y,
                )
                .to_simplex()
            } else {
                o.translate_by(&fr_geometry::planar::Vector::Int(
                    fr_geometry::planar::IntVector::new(location.x, location.y),
                ))
                .to_simplex()
            }
        }
        PadShape::Circle { radius } => {
            let circle = Circle::new(location, *radius);
            if orthogonal {
                circle.bounding_box().to_simplex()
            } else {
                circle.bounding_octagon().to_simplex()
            }
        }
    }
}

/// The board padstack matching a rules padstack, at the identity placement.
fn board_padstack_of(padstack: &RulesPadstack) -> Option<BoardPadstack> {
    let from_layer = padstack.from_layer();
    let to_layer = padstack.to_layer();
    if to_layer < from_layer {
        return None;
    }
    let shapes = (from_layer..=to_layer)
        .map(|layer| padstack.get_shape(layer).map(pad_shape_to_tile))
        .collect();
    Some(BoardPadstack {
        from_layer: from_layer as i32,
        to_layer: to_layer as i32,
        shapes,
        drill_radius: padstack.get_drill_radius().round() as i32,
    })
}

fn pad_shape_to_tile(pad: &PadShape) -> TileShape {
    match pad {
        PadShape::Box(b) => TileShape::Box(*b),
        PadShape::Octagon(o) => TileShape::Oct(*o),
        PadShape::Circle { radius } => {
            TileShape::Oct(Circle::new(IntPoint::new(0, 0), *radius).bounding_octagon())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_changed_area_is_empty_on_every_layer() {
        let area = ChangedArea::new(4);
        assert!(area.is_empty());
        for layer in 0..4 {
            assert!(area.area(layer).is_empty());
        }
    }

    #[test]
    fn joining_two_points_spans_them() {
        let mut area = ChangedArea::new(2);
        area.join_point(FloatPoint::new(-10.5, -20.5), 1);
        area.join_point(FloatPoint::new(30.5, 40.5), 1);
        // Layer 0 was never touched.
        assert!(area.area(0).is_empty());
        let octagon = area.area(1);
        // Minima floor, maxima ceil (`ChangedArea.java:118-124`).
        assert_eq!(octagon.left_x, -11);
        assert_eq!(octagon.bottom_y, -21);
        assert_eq!(octagon.right_x, 31);
        assert_eq!(octagon.top_y, 41);
        let bb = octagon.bounding_box();
        assert!(bb.left_x() <= -10 && bb.right_x() >= 30);
    }

    #[test]
    fn set_empty_clears_one_layer_only() {
        let mut area = ChangedArea::new(2);
        area.join_point(FloatPoint::new(0.0, 0.0), 0);
        area.join_point(FloatPoint::new(0.0, 0.0), 1);
        area.set_empty(0);
        assert!(area.area(0).is_empty());
        assert!(!area.area(1).is_empty());
    }

    #[test]
    fn out_of_range_layers_are_ignored_not_panicked_on() {
        let mut area = ChangedArea::new(1);
        area.join_point(FloatPoint::new(1.0, 1.0), 7);
        area.join_point(FloatPoint::new(1.0, 1.0), -1);
        assert!(area.is_empty());
        assert!(area.area(7).is_empty());
    }

    #[test]
    fn containment_rejects_a_shape_over_the_board_edge() {
        let bounds = IntBox::new(0, 0, 100, 100);
        assert!(fr_board::shove::geom::simplex_contained_in(
            &IntBox::new(10, 10, 90, 90).to_simplex(),
            &bounds
        ));
        assert!(!fr_board::shove::geom::simplex_contained_in(
            &IntBox::new(10, 10, 110, 90).to_simplex(),
            &bounds
        ));
        // An unbounded shape can never fit: a half plane is the smallest one.
        let half_plane = crate::geom::half_plane(&fr_geometry::planar::Line::new(
            IntPoint::new(0, 0),
            IntPoint::new(0, 10),
        ));
        assert!(!half_plane.is_empty());
        assert!(!fr_board::shove::geom::simplex_contained_in(
            &half_plane,
            &bounds
        ));
    }
}
