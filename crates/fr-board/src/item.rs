//! The board item model.
//!
//! Ported from `app.freerouting.board.model.items.*` following
//! `docs/upstream-study/v2-vs-v19-and-scope.md` §9 (the `Item` base, the four
//! fixed states, the obstacle matrix) and §12 (the area family), plus
//! `docs/upstream-study/drc-connectivity.md` §2.3 (the obstacle predicate as
//! the checker evaluates it) and §1.4 (what the checker needs per item type).
//!
//! # Shape of the port
//!
//! Upstream is a nine-class inheritance tree rooted at an abstract `Item`, with
//! the behavioral differences expressed as overrides of four predicates
//! (`isObstacle`, `isTraceObstacle`, `isDrillable`, `isRoutable`) and a handful
//! of geometry accessors. Here that becomes one [`Item`] struct holding the
//! fields the base class owns, plus an [`ItemKind`] enum holding the
//! per-subclass payload; the predicate overrides become `match` arms in one
//! place, which is the only way to read the obstacle matrix as a matrix.
//!
//! Three upstream fields are deliberately absent:
//!
//! * `transient BasicBoard board` — the back-pointer. Items are values in an
//!   [`ItemStore`](crate::store::ItemStore); the board is the caller.
//! * `boolean onTheBoard` — presence in the store *is* the predicate
//!   ([`ItemStore::contains`](crate::store::ItemStore::contains)). Upstream
//!   keeps a flag that `ShapeTraceEntries.fastCutoutTrace` sets by hand while
//!   bypassing the tree (`search-tree-obstacles.md` §6.3), which is exactly the
//!   kind of desync a single source of truth removes.
//! * `ItemSearchTreesInfo` / `ItemAutorouteInfo` — derived caches. They belong
//!   in slot-indexed side tables owned by the search-tree manager and the
//!   autoroute engine (see [`ItemStore::slot_of`](crate::store::ItemStore::slot_of)),
//!   not inside the item, so that a snapshot of the item store does not drag
//!   megabytes of recomputable tile shapes with it.
//!
//! `smallestClearance` (a memo of the tightest clearance seen during DRC) is
//! likewise a checker-side concern and is not stored here.

use fr_geometry::planar::{
    Direction, FloatPoint, IntBox, IntOctagon, IntPoint, IntVector, Line, Point, Polygon,
    Polyline, Simplex, Vector,
};

use crate::board::Board;
use crate::net_nos::NetNos;
use crate::shape::TraceExitRestriction;

/// Stable, monotonically increasing semantic identity of a board item.
///
/// This is *not* a storage slot. [`ItemStore`](crate::store::ItemStore) hands
/// out `ItemId`s from a counter that never decreases and never reuses a value —
/// not on removal, not on transaction rollback, not on history restore — while
/// the arena slot an item happens to live in is recycled freely. Two properties
/// depend on that separation:
///
/// * **Iteration order.** Upstream's `Item.compareTo` is `other.id - this.id`,
///   so every `TreeSet<Item>` and every board scan runs in *descending* id
///   order (`v2-vs-v19-and-scope.md` §9.1, §13). Routing results depend on it.
///   [`ItemStore::iter`](crate::store::ItemStore::iter) reproduces that order;
///   a slot index leaking into the ordering would silently break parity.
/// * **"Created by this operation".** `enforceStrictDrc` and several
///   diagnostics rely on `id > maxItemIdBefore`
///   (`pipeline/BatchAutorouter.java:305-329`), which is only sound if ids are
///   never recycled.
///
/// `ItemId(0)` is the unassigned sentinel; real ids start at 1, matching
/// upstream's 1-based id generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ItemId(u32);

impl ItemId {
    /// The sentinel for "this item has not been inserted yet".
    pub const UNASSIGNED: ItemId = ItemId(0);

    /// Wraps a raw id. Only the store and its snapshot/restore paths mint ids;
    /// this exists for tests and for IO that replays a stored board.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        ItemId(raw)
    }

    /// The raw numeric id, as it appears in upstream logs and DRC reports.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// True unless this is [`ItemId::UNASSIGNED`].
    #[inline]
    pub const fn is_assigned(self) -> bool {
        self.0 != 0
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// How immutable an item is, in upstream's ordinal order.
///
/// `UNFIXED < SHOVE_FIXED < USER_FIXED < SYSTEM_FIXED`
/// (`board/model/structure/FixedState.java`). The inline comments on
/// `SHOVE_FIXED` and `USER_FIXED` are swapped in both upstream trees — the
/// ordinal ordering is what the code actually tests, so that is what is ported
/// (`v2-vs-v19-and-scope.md` §9.2).
///
/// The derived [`Ord`] is the ordinal order, so `>=` comparisons read exactly
/// like the Java predicates.
///
/// | predicate | meaning |
/// |---|---|
/// | [`is_shove_fixed`](FixedState::is_shove_fixed) | `>= ShoveFixed`; the shove engine may not push it |
/// | [`is_user_fixed`](FixedState::is_user_fixed) | `>= UserFixed`; `removeItems` refuses it, `(type route)` copper imports at this level |
/// | [`unfix`](FixedState::unfix) | resets to `Unfixed` unless `SystemFixed` |
///
/// Two upstream predicates need context this enum does not have and therefore
/// live on [`Item`]: the `Trace` override of `isShoveFixed` (which also
/// consults the net class' `shoveFixed` flag) and `isDeletionForbidden` (which
/// consults the component id and, for conduction areas, the layer structure).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum FixedState {
    /// Freely routable, shovable and rippable.
    #[default]
    Unfixed,
    /// The shove engine must route around it; ripup may still delete it.
    ShoveFixed,
    /// User-protected: `(type route)` and `(type protect)` copper on import.
    UserFixed,
    /// Structural: pins, board outline, planes inserted by the importer.
    SystemFixed,
}

impl FixedState {
    /// Upstream `FixedState.isShoveFixed()` — `ordinal >= SHOVE_FIXED`.
    #[inline]
    pub fn is_shove_fixed(self) -> bool {
        self >= FixedState::ShoveFixed
    }

    /// Upstream `FixedState.isUserFixed()` — `ordinal >= USER_FIXED`.
    #[inline]
    pub fn is_user_fixed(self) -> bool {
        self >= FixedState::UserFixed
    }

    /// Upstream `Item.unfix()` — everything but `SYSTEM_FIXED` drops to
    /// `UNFIXED`.
    #[inline]
    pub fn unfix(self) -> FixedState {
        if self == FixedState::SystemFixed {
            self
        } else {
            FixedState::Unfixed
        }
    }
}

/// The inclusive layer span an item occupies.
///
/// Single-layer items (traces, areas) have `first == last`. For drill items
/// this is the padstack's `fromLayer() ..= toLayer()`; note that upstream's
/// `insertVia` splits traces over the **half-open** range while
/// `insertEscapeVia` uses the closed one (`v2-vs-v19-and-scope.md` §11.3) — a
/// quirk that belongs to the insert path, not to this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerRange {
    /// Lowest layer index occupied, inclusive.
    pub first: i32,
    /// Highest layer index occupied, inclusive.
    pub last: i32,
}

impl LayerRange {
    /// A span covering exactly one layer.
    #[inline]
    pub const fn single(layer: i32) -> Self {
        LayerRange {
            first: layer,
            last: layer,
        }
    }

    /// A span from `first` to `last`, inclusive.
    #[inline]
    pub const fn new(first: i32, last: i32) -> Self {
        LayerRange { first, last }
    }

    /// True when `layer` is inside the span.
    #[inline]
    pub const fn contains(&self, layer: i32) -> bool {
        layer >= self.first && layer <= self.last
    }

    /// True when the span covers exactly one layer — upstream's SMD test for
    /// pins (`Pin.drillAllowed()`).
    #[inline]
    pub const fn is_single(&self) -> bool {
        self.first == self.last
    }

    /// Number of layers spanned.
    #[inline]
    pub const fn count(&self) -> usize {
        if self.last < self.first {
            0
        } else {
            (self.last - self.first + 1) as usize
        }
    }
}

/// Convex-decomposable geometry of an area item.
///
/// Upstream areas hold a `geometry.planar.Area`, an interface whose
/// implementations range from a single `IntBox` to a `PolylineArea` with holes,
/// and derive their tree shapes through `splitToConvex()` — *derive*, not store:
/// `ObstacleArea` keeps the area it was constructed with, and the decomposition
/// is recomputed by whoever needs it. This enum works the same way, so
/// [`Polygon`](AreaShape::Polygon) holds the original corner list and
/// [`search_tree::placed_area_pieces`](crate::search_tree::placed_area_pieces)
/// splits it on demand; anything that has to write the area back out still has
/// its corners.
///
/// `fr-geometry` has no area-with-holes type yet, which is the one shape here
/// that cannot be spelled: an area with `(window …)` holes keeps only its
/// border. [`Convex`](AreaShape::Convex) is the escape hatch for a caller that
/// already holds a decomposition and has no polygon to go back to.
#[derive(Debug, Clone, PartialEq)]
pub enum AreaShape {
    /// Axis-parallel rectangle.
    Box(IntBox),
    /// 45°-bounded octagon.
    Octagon(IntOctagon),
    /// A single convex piece.
    Simplex(Simplex),
    /// A simple polygon, not necessarily convex — upstream's non-convex pad and
    /// keepout inputs (`dsn-board-semantics.md` §8.1).
    Polygon(Polygon),
    /// A pre-computed convex decomposition, i.e. the result of `splitToConvex()`.
    ///
    /// An empty decomposition is upstream's degenerate area, whose
    /// `tileShapeCount()` silently returns 0 and which is therefore invisible to
    /// the search tree (`v2-vs-v19-and-scope.md` §12.1).
    Convex(Vec<Simplex>),
}

/// The placed geometry of an area item: a shape in component-relative
/// coordinates plus the placement that puts it on the board.
///
/// Mirrors upstream `ObstacleArea`'s `relativeArea` / `translation` /
/// `rotationInDegree` / `sideChanged` quartet (§12.1). The transformed area is
/// derived, not stored: upstream caches it lazily inside the item, which is a
/// derived-data cache and belongs in a side table for the same reason the tree
/// shapes do.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedArea {
    /// The shape in component-relative coordinates.
    pub relative_area: AreaShape,
    /// Translation applied to `relative_area`.
    pub translation: IntVector,
    /// Rotation in degrees applied to `relative_area`.
    ///
    /// `f64` because upstream stores a `double` here and DSN carries fractional
    /// rotations. It is placement input, not a topology decision: the resulting
    /// area is integral, and no routing predicate reads this field.
    pub rotation_deg: f64,
    /// Whether the owning component is mirrored to the back side.
    pub side_changed: bool,
}

impl PlacedArea {
    /// An unplaced area: identity translation, no rotation, front side.
    pub fn new(relative_area: AreaShape) -> Self {
        PlacedArea {
            relative_area,
            translation: IntVector::new(0, 0),
            rotation_deg: 0.0,
            side_changed: false,
        }
    }

    /// Overwrites `self` with a deep copy of `src`, reusing `self`'s
    /// allocations; see [`Item::clone_from_ref`].
    pub fn clone_from_ref(&mut self, src: &PlacedArea) {
        self.translation = src.translation;
        self.rotation_deg = src.rotation_deg;
        self.side_changed = src.side_changed;
        match (&mut self.relative_area, &src.relative_area) {
            (AreaShape::Convex(dst), AreaShape::Convex(s)) => {
                dst.clear();
                dst.extend(s.iter().cloned());
            }
            (dst, s) => *dst = s.clone(),
        }
    }
}

/// Which of upstream's keepout subclasses an [`ObstacleArea`] is.
///
/// The three differ only in their obstacle predicates, which is why they are a
/// tag here rather than three item kinds (`v2-vs-v19-and-scope.md` §12.1-12.3,
/// `drc-connectivity.md` §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObstacleKind {
    /// `ObstacleArea` — blocks foreign-net traces and vias. Filter `KEEPOUT`.
    Keepout,
    /// `ViaObstacleArea` — blocks foreign-net vias only; never blocks traces.
    /// Filter `VIA_KEEPOUT`.
    ViaKeepout,
    /// `ComponentObstacleArea` — blocks only *other* components' placement
    /// keepouts; invisible to the router. Filter `COMPONENT_KEEPOUT`.
    ComponentKeepout,
}

/// A component pad (`board.model.items.Pin`).
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    /// Pad centre; the item's single connection point (§10.1).
    pub center: IntPoint,
    /// Index into the board's padstack table. Indices, not references: the
    /// padstack table lives in `fr-rules` and is shared by every item using it.
    pub padstack_no: u32,
    /// Index of this pin inside its component.
    pub pin_no: u32,
    /// Resolved padstack layer span. `first == last` means an SMD pad, which is
    /// exactly upstream's `drillAllowed()`.
    pub layers: LayerRange,
}

impl Pin {
    /// Returns the trace exit restrictions for this pin on `layer`.
    pub fn exit_restrictions<'a>(&self, board: &'a Board, layer: i32) -> &'a [TraceExitRestriction] {
        board
            .padstacks()
            .get(self.padstack_no)
            .map(|p| p.exit_restrictions_on_layer(layer))
            .unwrap_or(&[])
    }

    /// Calculates the nearest legal exit restriction direction for changing `trace_polyline`.
    /// Upstream `Pin.calc_nearest_exit_restriction_direction` (`:575-640`).
    pub fn calc_nearest_exit_restriction_direction(
        &self,
        board: &Board,
        trace_polyline: &Polyline,
        trace_half_width: i32,
        layer: i32,
        edge_to_turn_dist: f64,
    ) -> Option<Direction> {
        let restrictions = self.exit_restrictions(board, layer);
        if restrictions.is_empty() || edge_to_turn_dist < 0.0 {
            return None;
        }
        let padstack = board.padstacks().get(self.padstack_no)?;
        let pin_shape = padstack.shape_on_layer(layer)?;
        let offset = Vector::Int(self.center.difference_by_int(&IntPoint::ZERO));
        let absolute_pin_shape = pin_shape.translate_by(&offset);
        let offset_pin_shape = absolute_pin_shape.offset(edge_to_turn_dist + f64::from(trace_half_width));

        let entries = offset_pin_shape.entrance_points(trace_polyline);
        if entries.is_empty() {
            return None;
        }
        let latest_entry = entries.last().unwrap();
        let border_line = offset_pin_shape.border_line(latest_entry.1);
        let trace_entry_location = trace_polyline.arr[latest_entry.0].intersection_approx(&border_line);

        let mut min_dist = f64::MAX;
        let mut nearest_exit_dir = None;
        let mut nearest_exit_corner = None;
        const TOLERANCE: f64 = 1.0;

        for r in restrictions {
            let Some(border_no) = offset_pin_shape.intersecting_border_line_no(&self.center, &r.direction) else {
                continue;
            };
            let exit_ray = Line::new_from_point_and_dir(self.center, &r.direction);
            let exit_corner = exit_ray.intersection_approx(&offset_pin_shape.border_line(border_no));
            let corner_dist = exit_corner.distance_square(&trace_entry_location);
            let mut new_nearest = false;
            if corner_dist + TOLERANCE < min_dist {
                new_nearest = true;
            } else if corner_dist < min_dist + TOLERANCE {
                let old_corner = nearest_exit_corner.unwrap_or(exit_corner);
                for i in 1..trace_polyline.corner_count() {
                    let trace_corner = trace_polyline.corner_approx(i);
                    let d_new = trace_corner.distance_square(&exit_corner);
                    let d_old = trace_corner.distance_square(&old_corner);
                    if d_new + TOLERANCE < d_old {
                        new_nearest = true;
                        break;
                    } else if d_new > d_old + TOLERANCE {
                        break;
                    }
                }
            }
            if new_nearest {
                min_dist = corner_dist;
                nearest_exit_dir = Some(r.direction);
                nearest_exit_corner = Some(exit_corner);
            }
        }
        nearest_exit_dir
    }

    /// Calculates the nearest legal exit corner of the pin on `layer`.
    /// Upstream `Pin.nearest_trace_exit_corner` (`:645-680`).
    pub fn nearest_trace_exit_corner(
        &self,
        board: &Board,
        from_point: &FloatPoint,
        trace_half_width: i32,
        layer: i32,
        edge_to_turn_dist: f64,
    ) -> Option<FloatPoint> {
        let restrictions = self.exit_restrictions(board, layer);
        if restrictions.is_empty() || edge_to_turn_dist < 0.0 {
            return None;
        }
        let padstack = board.padstacks().get(self.padstack_no)?;
        let pin_shape = padstack.shape_on_layer(layer)?;
        let offset = Vector::Int(self.center.difference_by_int(&IntPoint::ZERO));
        let absolute_pin_shape = pin_shape.translate_by(&offset);
        let offset_pin_shape = absolute_pin_shape.offset(edge_to_turn_dist + f64::from(trace_half_width));

        let mut min_dist = f64::MAX;
        let mut nearest_exit_corner = None;
        for r in restrictions {
            let Some(border_no) = offset_pin_shape.intersecting_border_line_no(&self.center, &r.direction) else {
                continue;
            };
            let exit_ray = Line::new_from_point_and_dir(self.center, &r.direction);
            let exit_corner = exit_ray.intersection_approx(&offset_pin_shape.border_line(border_no));
            let d = exit_corner.distance_square(from_point);
            if d < min_dist {
                min_dist = d;
                nearest_exit_corner = Some(exit_corner);
            }
        }
        nearest_exit_corner
    }
}

/// A via (`board.model.items.Via`).
#[derive(Debug, Clone, PartialEq)]
pub struct Via {
    /// Drill centre; the item's single connection point.
    pub center: IntPoint,
    /// Index into the board's padstack table.
    pub padstack_no: u32,
    /// Resolved padstack layer span.
    pub layers: LayerRange,
    /// The DSN `attach` flag: may this via land on a same-net SMD pad? Read by
    /// the `Via` row of the obstacle matrix.
    pub attach_allowed: bool,
    /// v2-only: inserted by the fanout escalation phase inside an SMD pad.
    pub is_escape_via: bool,
    /// v2-only: the SMD layer whose violations the escape-via exemption drops
    /// (`drc-connectivity.md` §2.5). `-1` when not an escape via.
    pub escape_via_smd_layer: i32,
}

/// A routed trace (`board.model.items.PolylineTrace`).
#[derive(Debug, Clone, PartialEq)]
pub struct PolylineTrace {
    /// The centre line, stored as lines with corners derived — upstream's
    /// representation, and the one `split`/`combine` operate on.
    pub polyline: Polyline,
    /// The copper layer this trace runs on.
    pub layer: i32,
    /// Half the trace width, in board units.
    pub half_width: i32,
}

impl PolylineTrace {
    /// The first corner — one of the two connection points (§10.1).
    #[inline]
    pub fn first_corner(&self) -> Point {
        self.polyline.first_corner()
    }

    /// The last corner — the other connection point.
    #[inline]
    pub fn last_corner(&self) -> Point {
        self.polyline.last_corner()
    }

    /// Number of corners; upstream's `cornerCount()`.
    #[inline]
    pub fn corner_count(&self) -> usize {
        self.polyline.corner_count()
    }

    /// True when the trace has fewer than two corners or both ends coincide.
    ///
    /// Upstream's degenerate trace, which `insertTraceWithoutCleaning` only
    /// admits at `>= USER_FIXED` (§11.1 step 3) and which `normalize` deletes
    /// unless deletion is forbidden (§11.6). The insert path that consumes this
    /// lands with the trace split/merge wave.
    pub fn is_degenerate(&self) -> bool {
        self.polyline.corner_count() < 2 || self.first_corner() == self.last_corner()
    }
}

/// A keepout of one of the three upstream flavors.
#[derive(Debug, Clone, PartialEq)]
pub struct ObstacleArea {
    /// Placed geometry.
    pub area: PlacedArea,
    /// The single layer this keepout applies to.
    pub layer: i32,
    /// Which upstream subclass this is.
    pub kind: ObstacleKind,
}

/// A copper pour / power plane (`board.model.items.ConductionArea`).
#[derive(Debug, Clone, PartialEq)]
pub struct ConductionArea {
    /// Placed geometry.
    pub area: PlacedArea,
    /// The layer the pour lives on.
    pub layer: i32,
    /// "Regarded as obstacle to foreign-net traces and vias". DSN import sets
    /// this to `false` (`ARCHITECTURE.md`, parity quirks), so foreign nets route
    /// straight through an imported pour unless the board toggles it.
    pub is_obstacle: bool,
    /// Upstream's `isFilled`; render-only, kept for round-trip fidelity.
    pub is_filled: bool,
}

/// The board edge (`board.model.items.BoardOutline`).
///
/// Inserted by the board constructor from the outline shapes; the search tree
/// gives it special treatment — per-layer offset strips around every border
/// line, or the convex pieces of a generated keepout-outside-outline area
/// (`search-tree-obstacles.md` §3.6).
#[derive(Debug, Clone, PartialEq)]
pub struct BoardOutline {
    /// The outline border polylines, one per closed contour.
    pub borders: Vec<Polyline>,
    /// Whether a keepout covering everything outside the outline was generated.
    pub keepout_outside_outline: bool,
}

/// Component silkscreen / courtyard geometry
/// (`board.model.items.ComponentOutline`).
///
/// Pure metadata: `isObstacle` is hard `false` and `tileShapeCount()` is 0, so
/// it is invisible to both the router and the checker (§12.6). It exists so a
/// DSN round-trip does not lose it.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentOutline {
    /// Placed geometry.
    pub area: PlacedArea,
    /// True when the outline belongs to the front side of the board.
    pub is_front: bool,
}

/// The per-subclass payload of an [`Item`].
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// A component pad.
    Pin(Pin),
    /// A via.
    Via(Via),
    /// A routed trace.
    Trace(PolylineTrace),
    /// A keepout of one of the three flavors.
    ObstacleArea(ObstacleArea),
    /// A copper pour.
    ConductionArea(ConductionArea),
    /// The board edge.
    BoardOutline(BoardOutline),
    /// Component metadata geometry.
    ComponentOutline(ComponentOutline),
}

impl ItemKind {
    /// Allocation-reusing overwrite; see [`Item::clone_from_ref`].
    pub fn clone_from_ref(&mut self, src: &ItemKind) {
        match (&mut *self, src) {
            (ItemKind::Trace(dst), ItemKind::Trace(s)) => {
                dst.layer = s.layer;
                dst.half_width = s.half_width;
                // `Line` is `Copy`, so this is a memcpy into existing capacity.
                dst.polyline.arr.clear();
                dst.polyline.arr.extend_from_slice(&s.polyline.arr);
            }
            (ItemKind::ObstacleArea(dst), ItemKind::ObstacleArea(s)) => {
                dst.layer = s.layer;
                dst.kind = s.kind;
                dst.area.clone_from_ref(&s.area);
            }
            (ItemKind::ConductionArea(dst), ItemKind::ConductionArea(s)) => {
                dst.layer = s.layer;
                dst.is_obstacle = s.is_obstacle;
                dst.is_filled = s.is_filled;
                dst.area.clone_from_ref(&s.area);
            }
            (ItemKind::BoardOutline(dst), ItemKind::BoardOutline(s)) => {
                dst.keepout_outside_outline = s.keepout_outside_outline;
                dst.borders.clear();
                dst.borders.extend(s.borders.iter().cloned());
            }
            (ItemKind::ComponentOutline(dst), ItemKind::ComponentOutline(s)) => {
                dst.is_front = s.is_front;
                dst.area.clone_from_ref(&s.area);
            }
            // Pin and Via are plain-old-data; so is any mismatched pair, which
            // is rare enough not to be worth a per-field path.
            (dst, s) => *dst = s.clone(),
        }
    }
}

/// The nine leaves of upstream's item hierarchy, flattened to one discriminant.
///
/// Used wherever upstream writes `instanceof` — above all in the obstacle
/// matrix, where the *pair* of classes decides the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemClass {
    /// `Pin`
    Pin,
    /// `Via`
    Via,
    /// `PolylineTrace`
    Trace,
    /// `ObstacleArea`
    Keepout,
    /// `ViaObstacleArea`
    ViaKeepout,
    /// `ComponentObstacleArea`
    ComponentKeepout,
    /// `ConductionArea`
    ConductionArea,
    /// `BoardOutline`
    BoardOutline,
    /// `ComponentOutline`
    ComponentOutline,
}

impl ItemClass {
    /// True for the `ObstacleArea` inheritance branch — the three keepout
    /// flavors *and* `ConductionArea`, which extends `ObstacleArea` upstream.
    ///
    /// The distinction matters in two places: the `Pin` and the `BoardOutline`
    /// rows of the obstacle matrix. See [`Item::is_obstacle`].
    #[inline]
    pub fn is_obstacle_area_family(self) -> bool {
        matches!(
            self,
            ItemClass::Keepout
                | ItemClass::ViaKeepout
                | ItemClass::ComponentKeepout
                | ItemClass::ConductionArea
        )
    }

    /// True for the item classes that implement upstream's `Connectable`:
    /// pins, vias, traces and conduction areas (`drc-connectivity.md` §4.1).
    #[inline]
    pub fn is_connectable(self) -> bool {
        matches!(
            self,
            ItemClass::Pin | ItemClass::Via | ItemClass::Trace | ItemClass::ConductionArea
        )
    }
}

/// A board item: the fields upstream's abstract `Item` owns, plus the subclass
/// payload.
///
/// See the module docs for what is deliberately *not* here.
#[derive(Debug, PartialEq)]
pub struct Item {
    /// Semantic identity. Assigned by
    /// [`ItemStore::insert`](crate::store::ItemStore::insert); the store is the
    /// only authority, and it overwrites whatever the caller put here.
    pub id: ItemId,
    /// The nets this item belongs to, in upstream's array order.
    pub nets: NetNos,
    /// Index into the clearance matrix. `i32` to match upstream, including the
    /// out-of-range behavior the board insert path implements (warn, force 0).
    pub clearance_class: i32,
    /// Owning component, `0` when the item is not part of a component. A
    /// non-zero value makes the item undeletable.
    pub component_no: i32,
    /// How immutable this item is.
    pub fixed_state: FixedState,
    /// The subclass payload.
    pub kind: ItemKind,
}

impl Item {
    /// Creates an item that is not yet in a store.
    pub fn new(
        nets: NetNos,
        clearance_class: i32,
        fixed_state: FixedState,
        kind: ItemKind,
    ) -> Self {
        Item {
            id: ItemId::UNASSIGNED,
            nets,
            clearance_class,
            component_no: 0,
            fixed_state,
            kind,
        }
    }

    /// Sets the owning component (builder style).
    pub fn with_component(mut self, component_no: i32) -> Self {
        self.component_no = component_no;
        self
    }

    /// The flattened class discriminant.
    pub fn class(&self) -> ItemClass {
        match &self.kind {
            ItemKind::Pin(_) => ItemClass::Pin,
            ItemKind::Via(_) => ItemClass::Via,
            ItemKind::Trace(_) => ItemClass::Trace,
            ItemKind::ObstacleArea(a) => match a.kind {
                ObstacleKind::Keepout => ItemClass::Keepout,
                ObstacleKind::ViaKeepout => ItemClass::ViaKeepout,
                ObstacleKind::ComponentKeepout => ItemClass::ComponentKeepout,
            },
            ItemKind::ConductionArea(_) => ItemClass::ConductionArea,
            ItemKind::BoardOutline(_) => ItemClass::BoardOutline,
            ItemKind::ComponentOutline(_) => ItemClass::ComponentOutline,
        }
    }

    // ---- nets ------------------------------------------------------------

    /// Upstream `Item.containsNet(int)`: **false for `net_no <= 0`**, because
    /// net numbers are 1-based and 0 means "no net" (§9.1).
    #[inline]
    pub fn contains_net(&self, net_no: i32) -> bool {
        net_no > 0 && self.nets.contains(net_no)
    }

    /// Upstream `Item.sharesNet(Item)`.
    #[inline]
    pub fn shares_net(&self, other: &Item) -> bool {
        self.nets.shares_any(&other.nets)
    }

    /// Number of nets this item belongs to.
    #[inline]
    pub fn net_count(&self) -> usize {
        self.nets.len()
    }

    /// Upstream `Item.netsNormal()` — every net number is a real net.
    #[inline]
    pub fn nets_normal(&self) -> bool {
        self.nets.all_normal()
    }

    // ---- fixed state -----------------------------------------------------

    /// Base `isShoveFixed()`: `fixedState >= ShoveFixed`.
    ///
    /// `Trace` overrides this upstream to also return true when one of its nets
    /// belongs to a net class with `shoveFixed` (`Trace.java:238-253`). That
    /// half needs the rules model, which this crate does not depend on — use
    /// [`is_shove_fixed_by`](Item::is_shove_fixed_by) where the net classes are
    /// in scope.
    #[inline]
    pub fn is_shove_fixed(&self) -> bool {
        self.fixed_state.is_shove_fixed()
    }

    /// The full upstream `isShoveFixed()`, including the `Trace` override.
    ///
    /// `net_class_shove_fixed(net_no)` answers "does this net's class set
    /// `shoveFixed`".
    pub fn is_shove_fixed_by(&self, net_class_shove_fixed: impl Fn(i32) -> bool) -> bool {
        if self.fixed_state.is_shove_fixed() {
            return true;
        }
        matches!(self.kind, ItemKind::Trace(_))
            && self
                .nets
                .iter()
                .any(|n| *n > 0 && net_class_shove_fixed(*n))
    }

    /// Upstream `isUserFixed()`: `fixedState >= UserFixed`.
    #[inline]
    pub fn is_user_fixed(&self) -> bool {
        self.fixed_state.is_user_fixed()
    }

    /// Upstream `Item.isDeletionForbidden()` (`Item.java:828-830`):
    /// `componentId > 0 || isUserFixed() || (ConductionArea on a non-signal layer)`.
    ///
    /// `layer_is_signal(layer)` supplies the third term; the layer structure
    /// belongs to the board, not to the item.
    pub fn is_deletion_forbidden(&self, layer_is_signal: impl Fn(i32) -> bool) -> bool {
        if self.component_no > 0 || self.is_user_fixed() {
            return true;
        }
        match &self.kind {
            ItemKind::ConductionArea(a) => !layer_is_signal(a.layer),
            _ => false,
        }
    }

    /// [`is_deletion_forbidden`](Item::is_deletion_forbidden) for callers with
    /// no layer structure at hand, i.e. treating every layer as a signal layer.
    ///
    /// Correct for every item class except a conduction area on a plane layer,
    /// which this reports as deletable where upstream would not.
    #[inline]
    pub fn is_deletion_forbidden_ignoring_layers(&self) -> bool {
        self.is_deletion_forbidden(|_| true)
    }

    /// Upstream `Item.isRoutable()`: base `false`; a trace or via is routable
    /// when it is not user-fixed and carries at least one net (§9.2).
    pub fn is_routable(&self) -> bool {
        matches!(self.kind, ItemKind::Trace(_) | ItemKind::Via(_))
            && !self.is_user_fixed()
            && self.net_count() > 0
    }

    /// Upstream `Item.unfix()`.
    #[inline]
    pub fn unfix(&mut self) {
        self.fixed_state = self.fixed_state.unfix();
    }

    // ---- layers ----------------------------------------------------------

    /// The inclusive layer span this item occupies.
    pub fn layer_range(&self) -> LayerRange {
        match &self.kind {
            ItemKind::Pin(p) => p.layers,
            ItemKind::Via(v) => v.layers,
            ItemKind::Trace(t) => LayerRange::single(t.layer),
            ItemKind::ObstacleArea(a) => LayerRange::single(a.layer),
            ItemKind::ConductionArea(a) => LayerRange::single(a.layer),
            // Both outlines are layer-independent; the tree replicates the board
            // outline's shapes per layer (`search-tree-obstacles.md` §3.6) and
            // gives the component outline none at all.
            ItemKind::BoardOutline(_) | ItemKind::ComponentOutline(_) => {
                LayerRange::new(i32::MIN, i32::MAX)
            }
        }
    }

    /// Lowest occupied layer — upstream `firstLayer()`.
    #[inline]
    pub fn first_layer(&self) -> i32 {
        self.layer_range().first
    }

    /// Highest occupied layer — upstream `lastLayer()`.
    #[inline]
    pub fn last_layer(&self) -> i32 {
        self.layer_range().last
    }

    /// True when the item has copper (or a keepout) on `layer`.
    #[inline]
    pub fn is_on_layer(&self, layer: i32) -> bool {
        self.layer_range().contains(layer)
    }

    /// Upstream `Pin.drillAllowed()`: only an SMD pad (`firstLayer() ==
    /// lastLayer()`) admits a via landing on it. `false` for every other class.
    #[inline]
    pub fn drill_allowed(&self) -> bool {
        match &self.kind {
            ItemKind::Pin(p) => p.layers.is_single(),
            _ => false,
        }
    }

    /// True for the classes implementing upstream's `Connectable`.
    #[inline]
    pub fn is_connectable(&self) -> bool {
        self.class().is_connectable()
    }

    // ---- the obstacle matrix --------------------------------------------

    /// Upstream `this.isObstacle(other)` — **asymmetric**, and the checker
    /// evaluates it on the *candidate* with the item under test as the
    /// argument: `candidate.is_obstacle(item_under_test)`
    /// (`drc-connectivity.md` §2.3, and note the direction warning there).
    ///
    /// Because the predicate is asymmetric and the checker's outer loop runs
    /// over every item, an unordered pair is examined twice and the two
    /// evaluations may disagree; upstream's reporting layer deduplicates and
    /// keeps whichever direction it saw first.
    ///
    /// The `self == other` tests compare [`ItemId`]s, which is what makes them
    /// meaningful for values rather than references. Two unassigned ids compare
    /// equal, so this predicate is only well defined for items that live in a
    /// store.
    pub fn is_obstacle(&self, other: &Item) -> bool {
        let same = self.id == other.id;
        let other_class = other.class();
        match &self.kind {
            // Trace.java:92-102
            ItemKind::Trace(_) => {
                if same
                    || other_class == ItemClass::ViaKeepout
                    || other_class == ItemClass::ComponentKeepout
                {
                    return false;
                }
                if let ItemKind::ConductionArea(a) = &other.kind {
                    if !a.is_obstacle {
                        return false;
                    }
                }
                !other.shares_net(self)
            }
            // Via.java:152-166
            ItemKind::Via(v) => {
                if same || other_class == ItemClass::ComponentKeepout {
                    return false;
                }
                if let ItemKind::ConductionArea(a) = &other.kind {
                    if !a.is_obstacle {
                        return false;
                    }
                }
                if !other.shares_net(self) {
                    return true;
                }
                if other_class == ItemClass::Trace {
                    return false;
                }
                !v.attach_allowed || other_class != ItemClass::Pin || !other.drill_allowed()
            }
            // Pin.java:353-365. `ObstacleArea` here includes `ConductionArea`,
            // which extends it upstream.
            ItemKind::Pin(p) => {
                if same || other_class.is_obstacle_area_family() {
                    return false;
                }
                if !other.shares_net(self) {
                    return true;
                }
                if other_class == ItemClass::Trace {
                    return false;
                }
                // v19 additionally required `|| !((Via) other).attachAllowed`;
                // v2 dropped that term and the release jar runs v2 here.
                !p.layers.is_single() || other_class != ItemClass::Via
            }
            ItemKind::ObstacleArea(a) => match a.kind {
                // ObstacleArea.java:175-180
                ObstacleKind::Keepout => {
                    !self.shares_net(other)
                        && matches!(other_class, ItemClass::Trace | ItemClass::Via)
                }
                // ViaObstacleArea.java:92-97
                ObstacleKind::ViaKeepout => {
                    !self.shares_net(other) && other_class == ItemClass::Via
                }
                // ComponentObstacleArea.java:64-69 — a full override of the
                // superclass, so it is net-independent: no shared-net exit here.
                ObstacleKind::ComponentKeepout => {
                    !same
                        && other_class == ItemClass::ComponentKeepout
                        && other.component_no != self.component_no
                }
            },
            // ConductionArea.java:380-386 — the ObstacleArea rule, gated on the
            // pour actually being declared an obstacle.
            ItemKind::ConductionArea(a) => {
                if !a.is_obstacle || self.shares_net(other) {
                    return false;
                }
                matches!(other_class, ItemClass::Trace | ItemClass::Via)
            }
            // BoardOutline.java:84-87.
            //
            // NOTE: `drc-connectivity.md` §2.3 quotes the code as
            // `!(self is BoardOutline || self is ObstacleArea)`, but its prose
            // then says the outline "clears traces, vias, pins and conduction
            // areas". Those disagree, because `ConductionArea extends
            // ObstacleArea`. The quoted code line wins (ARCHITECTURE: the
            // release jar is the reference), so a pour is *not* cleared from the
            // board edge here. Flagged for the conformance pass.
            ItemKind::BoardOutline(_) => {
                !(other_class == ItemClass::BoardOutline || other_class.is_obstacle_area_family())
            }
            // ComponentOutline.java:120-122 — always false, and it contributes
            // zero tree shapes anyway.
            ItemKind::ComponentOutline(_) => false,
        }
    }

    /// Upstream `Item.isTraceObstacle(int)` — the routing-side gate.
    ///
    /// Base is `!containsNet(net)`; `ViaObstacleArea` and
    /// `ComponentObstacleArea` are hard `false` (they never block a trace), and
    /// `ConductionArea` is `isObstacle && !containsNet(net)`.
    pub fn is_trace_obstacle(&self, net_no: i32) -> bool {
        match &self.kind {
            ItemKind::ObstacleArea(a) => match a.kind {
                ObstacleKind::Keepout => !self.contains_net(net_no),
                ObstacleKind::ViaKeepout | ObstacleKind::ComponentKeepout => false,
            },
            ItemKind::ConductionArea(a) => a.is_obstacle && !self.contains_net(net_no),
            // Zero tree shapes, so upstream never reaches its base
            // implementation for this class; answering `false` keeps callers
            // that do not consult the shape count honest.
            ItemKind::ComponentOutline(_) => false,
            _ => !self.contains_net(net_no),
        }
    }

    /// Upstream `Item.isDrillable(int)` — may a via of `net_no` be dropped
    /// through this item?
    ///
    /// Base is `false`; `Trace` returns `containsNet(net)` and
    /// `ConductionArea` returns `!isObstacle || containsNet(net)` (a
    /// non-obstacle pour is always drillable).
    pub fn is_drillable(&self, net_no: i32) -> bool {
        match &self.kind {
            ItemKind::Trace(_) => self.contains_net(net_no),
            ItemKind::ConductionArea(a) => !a.is_obstacle || self.contains_net(net_no),
            _ => false,
        }
    }

    // ---- pooled cloning --------------------------------------------------

    /// Overwrites `self` with a deep copy of `src`, reusing `self`'s
    /// allocations wherever the two agree in shape.
    ///
    /// This is what makes [`BoardHistory`](crate::history::BoardHistory)
    /// snapshots cheap: the pooled destination buffer already holds an item of
    /// the same kind at the same slot in the overwhelming majority of cases, so
    /// the polyline's `Vec<Line>` and an area's convex decomposition are
    /// memcpy'd into existing capacity instead of being reallocated.
    ///
    /// `#[derive(Clone)]` would give `clone_from` the default body
    /// (`*self = source.clone()`), which allocates for every trace on every
    /// snapshot — hence the hand-written impl.
    pub fn clone_from_ref(&mut self, src: &Item) {
        self.id = src.id;
        self.nets.clone_from_ref(&src.nets);
        self.clearance_class = src.clearance_class;
        self.component_no = src.component_no;
        self.fixed_state = src.fixed_state;
        self.kind.clone_from_ref(&src.kind);
    }
}

impl Clone for Item {
    fn clone(&self) -> Self {
        Item {
            id: self.id,
            nets: self.nets.clone(),
            clearance_class: self.clearance_class,
            component_no: self.component_no,
            fixed_state: self.fixed_state,
            kind: self.kind.clone(),
        }
    }

    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.clone_from_ref(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(id: u32, nets: &[i32], layer: i32) -> Item {
        let mut item = Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Trace(PolylineTrace {
                polyline: Polyline::from_int_points(&[IntPoint::new(0, 0), IntPoint::new(1000, 0)]),
                layer,
                half_width: 50,
            }),
        );
        item.id = ItemId::from_raw(id);
        item
    }

    fn pin(id: u32, nets: &[i32], layers: LayerRange) -> Item {
        let mut item = Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::SystemFixed,
            ItemKind::Pin(Pin {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                pin_no: 0,
                layers,
            }),
        );
        item.id = ItemId::from_raw(id);
        item
    }

    fn via(id: u32, nets: &[i32], attach_allowed: bool) -> Item {
        let mut item = Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::Unfixed,
            ItemKind::Via(Via {
                center: IntPoint::new(0, 0),
                padstack_no: 0,
                layers: LayerRange::new(0, 3),
                attach_allowed,
                is_escape_via: false,
                escape_via_smd_layer: -1,
            }),
        );
        item.id = ItemId::from_raw(id);
        item
    }

    fn keepout(id: u32, nets: &[i32], kind: ObstacleKind, component_no: i32) -> Item {
        let mut item = Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::SystemFixed,
            ItemKind::ObstacleArea(ObstacleArea {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(0, 0, 100, 100))),
                layer: 0,
                kind,
            }),
        );
        item.id = ItemId::from_raw(id);
        item.component_no = component_no;
        item
    }

    fn pour(id: u32, nets: &[i32], is_obstacle: bool) -> Item {
        let mut item = Item::new(
            NetNos::from_slice(nets),
            1,
            FixedState::SystemFixed,
            ItemKind::ConductionArea(ConductionArea {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(0, 0, 100, 100))),
                layer: 0,
                is_obstacle,
                is_filled: true,
            }),
        );
        item.id = ItemId::from_raw(id);
        item
    }

    fn outline(id: u32) -> Item {
        let mut item = Item::new(
            NetNos::EMPTY,
            0,
            FixedState::SystemFixed,
            ItemKind::BoardOutline(BoardOutline {
                borders: Vec::new(),
                keepout_outside_outline: false,
            }),
        );
        item.id = ItemId::from_raw(id);
        item
    }

    #[test]
    fn fixed_state_ordering_matches_upstream_ordinals() {
        assert!(FixedState::Unfixed < FixedState::ShoveFixed);
        assert!(FixedState::ShoveFixed < FixedState::UserFixed);
        assert!(FixedState::UserFixed < FixedState::SystemFixed);

        assert!(!FixedState::Unfixed.is_shove_fixed());
        assert!(FixedState::ShoveFixed.is_shove_fixed());
        assert!(FixedState::SystemFixed.is_shove_fixed());

        assert!(!FixedState::ShoveFixed.is_user_fixed());
        assert!(FixedState::UserFixed.is_user_fixed());

        assert_eq!(FixedState::UserFixed.unfix(), FixedState::Unfixed);
        assert_eq!(FixedState::SystemFixed.unfix(), FixedState::SystemFixed);
    }

    #[test]
    fn contains_net_rejects_the_no_net_marker() {
        let t = trace(1, &[0], 0);
        assert!(!t.contains_net(0));
        assert!(!t.contains_net(-1));
        assert!(!trace(2, &[3], 0).contains_net(4));
        assert!(trace(3, &[3], 0).contains_net(3));
    }

    #[test]
    fn trace_row_of_the_obstacle_matrix() {
        let t = trace(1, &[1], 0);
        assert!(!t.is_obstacle(&t));
        assert!(t.is_obstacle(&trace(2, &[2], 0)));
        assert!(!t.is_obstacle(&trace(2, &[1], 0)));
        // via keepouts and component keepouts never block traces
        assert!(!t.is_obstacle(&keepout(3, &[], ObstacleKind::ViaKeepout, 0)));
        assert!(!t.is_obstacle(&keepout(4, &[], ObstacleKind::ComponentKeepout, 0)));
        // a non-obstacle pour is transparent, an obstacle pour is not
        assert!(!t.is_obstacle(&pour(5, &[2], false)));
        assert!(t.is_obstacle(&pour(6, &[2], true)));
    }

    #[test]
    fn same_net_via_on_a_through_hole_pin_is_a_violation_but_smd_is_not() {
        // drc-connectivity.md §2.3, "same-net exclusion summary".
        let v = via(1, &[1], true);
        let smd = pin(2, &[1], LayerRange::single(0));
        let through_hole = pin(3, &[1], LayerRange::new(0, 3));
        assert!(!v.is_obstacle(&smd));
        assert!(v.is_obstacle(&through_hole));

        // and the same pair seen from the pin side
        assert!(!smd.is_obstacle(&v));
        assert!(through_hole.is_obstacle(&v));
    }

    #[test]
    fn a_via_without_attach_permission_conflicts_even_with_a_same_net_smd_pad() {
        let v = via(1, &[1], false);
        let smd = pin(2, &[1], LayerRange::single(0));
        assert!(v.is_obstacle(&smd));
        // The pin row has no `attachAllowed` term in v2, so the reverse
        // direction disagrees — exactly the asymmetry §2.3 warns about.
        assert!(!smd.is_obstacle(&v));
    }

    #[test]
    fn pin_is_never_obstructed_by_any_area_including_a_pour() {
        let p = pin(1, &[1], LayerRange::new(0, 3));
        assert!(!p.is_obstacle(&keepout(2, &[9], ObstacleKind::Keepout, 0)));
        assert!(!p.is_obstacle(&pour(3, &[9], true)));
    }

    #[test]
    fn component_keepouts_only_block_other_components_keepouts() {
        let a = keepout(1, &[], ObstacleKind::ComponentKeepout, 7);
        let same_component = keepout(2, &[], ObstacleKind::ComponentKeepout, 7);
        let other_component = keepout(3, &[], ObstacleKind::ComponentKeepout, 8);
        assert!(!a.is_obstacle(&same_component));
        assert!(a.is_obstacle(&other_component));
        assert!(!a.is_obstacle(&trace(4, &[1], 0)));
        assert!(!a.is_obstacle(&a));
    }

    #[test]
    fn board_outline_clears_copper_but_not_the_obstacle_area_family() {
        let o = outline(1);
        assert!(o.is_obstacle(&trace(2, &[1], 0)));
        assert!(o.is_obstacle(&via(3, &[1], true)));
        assert!(o.is_obstacle(&pin(4, &[1], LayerRange::single(0))));
        assert!(!o.is_obstacle(&outline(5)));
        assert!(!o.is_obstacle(&keepout(6, &[], ObstacleKind::Keepout, 0)));
        // See the note on the BoardOutline arm: the quoted code line puts
        // ConductionArea in the ObstacleArea family, so a pour is not cleared.
        assert!(!o.is_obstacle(&pour(7, &[1], true)));
    }

    #[test]
    fn component_outline_is_invisible() {
        let mut co = Item::new(
            NetNos::EMPTY,
            0,
            FixedState::SystemFixed,
            ItemKind::ComponentOutline(ComponentOutline {
                area: PlacedArea::new(AreaShape::Box(IntBox::new(0, 0, 10, 10))),
                is_front: true,
            }),
        );
        co.id = ItemId::from_raw(1);
        assert!(!co.is_obstacle(&trace(2, &[1], 0)));
        assert!(!co.is_trace_obstacle(1));
        assert!(!co.is_drillable(1));
    }

    #[test]
    fn routing_side_gates() {
        assert!(trace(1, &[1], 0).is_trace_obstacle(2));
        assert!(!trace(1, &[1], 0).is_trace_obstacle(1));
        assert!(!keepout(2, &[], ObstacleKind::ViaKeepout, 0).is_trace_obstacle(1));
        assert!(keepout(3, &[], ObstacleKind::Keepout, 0).is_trace_obstacle(1));

        assert!(!pour(4, &[1], false).is_trace_obstacle(2));
        assert!(pour(5, &[1], true).is_trace_obstacle(2));

        assert!(trace(6, &[1], 0).is_drillable(1));
        assert!(!trace(6, &[1], 0).is_drillable(2));
        assert!(pour(7, &[1], false).is_drillable(2));
        assert!(!pour(8, &[1], true).is_drillable(2));
        assert!(pour(9, &[1], true).is_drillable(1));
        assert!(!pin(10, &[1], LayerRange::single(0)).is_drillable(1));
    }

    #[test]
    fn routable_and_deletion_rules() {
        assert!(trace(1, &[1], 0).is_routable());
        assert!(!trace(2, &[], 0).is_routable());

        let mut fixed = trace(3, &[1], 0);
        fixed.fixed_state = FixedState::UserFixed;
        assert!(!fixed.is_routable());
        assert!(fixed.is_deletion_forbidden_ignoring_layers());

        assert!(!pin(4, &[1], LayerRange::single(0)).is_routable());

        let mut in_component = trace(5, &[1], 0);
        in_component.component_no = 3;
        assert!(in_component.is_deletion_forbidden_ignoring_layers());

        // A pour on a plane layer is undeletable, on a signal layer deletable —
        // with the other two terms of the predicate cleared, since the importer
        // marks pours SYSTEM_FIXED and that alone forbids deletion.
        let mut p = pour(6, &[1], true);
        p.fixed_state = FixedState::Unfixed;
        assert!(p.is_deletion_forbidden(|_| false));
        assert!(!p.is_deletion_forbidden(|_| true));
        p.fixed_state = FixedState::SystemFixed;
        assert!(p.is_deletion_forbidden(|_| true));
    }

    #[test]
    fn shove_fixed_consults_the_net_class_for_traces_only() {
        let t = trace(1, &[7], 0);
        assert!(!t.is_shove_fixed());
        assert!(t.is_shove_fixed_by(|net| net == 7));
        assert!(!t.is_shove_fixed_by(|net| net == 8));

        let v = via(2, &[7], true);
        assert!(!v.is_shove_fixed_by(|net| net == 7));
    }

    #[test]
    fn clone_from_ref_reuses_the_polyline_buffer() {
        let mut dst = trace(1, &[1], 0);
        // Give the destination enough capacity that the copy cannot realloc.
        if let ItemKind::Trace(t) = &mut dst.kind {
            t.polyline.arr.reserve(16);
        }
        let buffer_before = match &dst.kind {
            ItemKind::Trace(t) => t.polyline.arr.as_ptr(),
            _ => unreachable!(),
        };

        let mut src = trace(2, &[4, 5], 3);
        if let ItemKind::Trace(t) = &mut src.kind {
            t.polyline = Polyline::from_int_points(&[
                IntPoint::new(0, 0),
                IntPoint::new(500, 0),
                IntPoint::new(500, 500),
            ]);
            t.half_width = 77;
        }

        dst.clone_from_ref(&src);
        assert_eq!(dst, src);
        match &dst.kind {
            ItemKind::Trace(t) => assert_eq!(t.polyline.arr.as_ptr(), buffer_before),
            _ => unreachable!(),
        }
    }
}
