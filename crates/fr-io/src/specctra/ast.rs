//! The DSN abstract syntax tree, the coordinate transform, and the shape→board geometry
//! conversion.
//!
//! `read.rs` fills these types, `build.rs` consumes them. The split is the one structural change
//! `dsn-board-semantics.md` §17.4 insists on: upstream constructs the board halfway through
//! `Structure.readScope` and inserts items halfway through `Network.readScope`, which is exactly
//! why its cross-scope state bag exists and why the `autoroute_settings` ordering bug is
//! possible. Here the parse is a pure `bytes → Ast` function.
//!
//! Two things resist the split and are therefore kept as read-time state (see
//! [`crate::specctra::read`]): the DSN-side layer structure, because shape scopes resolve layer
//! names to indices while the structure scope is still being read, and the `string_quote`, which
//! `Structure.setClearanceRule` re-parses class-pair names with.

use fr_geometry::planar::{
    Circle, FloatPoint, IntBox, IntOctagon, IntPoint, Line, Point, Polygon, Simplex, Vector,
};

use super::jround::JavaRound;
use super::transform::CoordinateTransform;

// ---------------------------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------------------------

/// A DSN-side layer: upstream `io/specctra/parser/Layer.java`.
///
/// `no == -1` marks the two pseudo layers `pcb` ("all layers") and `signal` ("all signal
/// layers"); every shape scope may name either instead of a physical layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnLayer {
    /// Name exactly as spelled in the file.
    pub name: String,
    /// Index into the board layer stack, or `-1` for a pseudo layer.
    pub no: i32,
    /// False only for `(type power)`; `(type jumper)` counts as signal.
    pub is_signal: bool,
    /// Plane net names from `(use_net …)`. Empty for signal layers in practice.
    pub net_names: Vec<String>,
}

impl DsnLayer {
    /// The `pcb` pseudo layer.
    pub fn pcb() -> DsnLayer {
        DsnLayer {
            name: "pcb".to_string(),
            no: -1,
            is_signal: false,
            net_names: Vec::new(),
        }
    }

    /// The `signal` pseudo layer.
    pub fn signal() -> DsnLayer {
        DsnLayer {
            name: "signal".to_string(),
            no: -1,
            is_signal: true,
            net_names: Vec::new(),
        }
    }

    /// True for the `pcb` pseudo layer.
    pub fn is_pcb_pseudo(&self) -> bool {
        self.no < 0 && !self.is_signal
    }

    /// True for the `signal` pseudo layer.
    pub fn is_signal_pseudo(&self) -> bool {
        self.no < 0 && self.is_signal
    }
}

/// The DSN-side layer stack, rebuilt every time a `(layer …)` scope is read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DsnLayerStructure {
    /// The physical layers, in file order.
    pub layers: Vec<DsnLayer>,
}

impl DsnLayerStructure {
    /// Upstream `LayerStructure.getNo`.
    ///
    /// Exact, **case-sensitive** name match first, then the Electra fallback: any name
    /// *containing* `Top` is layer 0 and any name containing `Bottom` is the last layer. The
    /// fallback is why a layer literally named `Stopper` resolves to layer 0.
    pub fn get_no(&self, name: &str) -> i32 {
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.name == name {
                return index as i32;
            }
        }
        if name.contains("Top") {
            return 0;
        }
        if name.contains("Bottom") {
            return self.layers.len() as i32 - 1;
        }
        -1
    }

    /// Upstream `LayerStructure.containsPlane`: true iff some **non-signal** layer lists
    /// `net_name` in its `(use_net …)`.
    pub fn contains_plane(&self, net_name: &str) -> bool {
        self.layers
            .iter()
            .any(|layer| !layer.is_signal && layer.net_names.iter().any(|n| n == net_name))
    }

    /// Number of layers.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// True when no layer has been read yet.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------------------------

/// A DSN shape scope, before any coordinate transform.
///
/// Upstream is an abstract `Shape` with five subclasses and `instanceof` dispatch at half a dozen
/// call sites; the enum makes the dispatch a `match` and removes the vtable
/// (`dsn-board-semantics.md` §17.2).
#[derive(Debug, Clone, PartialEq)]
pub enum DsnShape {
    /// `(rect <layer> llx lly urx ury)`.
    Rect {
        /// Resolved layer.
        layer: DsnLayer,
        /// The four corner coordinates, in file order.
        coor: [f64; 4],
    },
    /// `(circle <layer> <diameter> [cx cy])` — note the first number is a **diameter**.
    Circle {
        /// Resolved layer.
        layer: DsnLayer,
        /// `[diameter, cx, cy]`; missing coordinates default to 0.
        coor: [f64; 3],
    },
    /// `(polygon <layer> <aperture> x y …)`.
    Polygon {
        /// Resolved layer.
        layer: DsnLayer,
        /// Flat `x0 y0 x1 y1 …` list. The aperture width is read and discarded upstream.
        coor: Vec<f64>,
    },
    /// `(path <layer> <width> x y …)` — a corner list.
    PolygonPath {
        /// Resolved layer.
        layer: DsnLayer,
        /// Path width in DSN units.
        width: f64,
        /// Flat `x0 y0 x1 y1 …` list.
        coor: Vec<f64>,
    },
    /// `(polyline_path <layer> <width> ax ay bx by …)` — a *line* list, four numbers per line.
    PolylinePath {
        /// Resolved layer.
        layer: DsnLayer,
        /// Path width in DSN units.
        width: f64,
        /// Flat `ax ay bx by …` list.
        coor: Vec<f64>,
    },
}

impl DsnShape {
    /// The layer this shape names.
    pub fn layer(&self) -> &DsnLayer {
        match self {
            DsnShape::Rect { layer, .. }
            | DsnShape::Circle { layer, .. }
            | DsnShape::Polygon { layer, .. }
            | DsnShape::PolygonPath { layer, .. }
            | DsnShape::PolylinePath { layer, .. } => layer,
        }
    }

    /// True for the two `Path` subclasses, which `Structure.addBoundaryShape` always files as
    /// outline shapes regardless of their layer.
    pub fn is_path(&self) -> bool {
        matches!(
            self,
            DsnShape::PolygonPath { .. } | DsnShape::PolylinePath { .. }
        )
    }

    /// The path width, or `0.0` for the non-path shapes — upstream's `Path.width` versus the
    /// `0.0` `Library.readScope` substitutes for polygons and rectangles.
    pub fn path_width(&self) -> f64 {
        match self {
            DsnShape::PolygonPath { width, .. } | DsnShape::PolylinePath { width, .. } => *width,
            _ => 0.0,
        }
    }

    /// Upstream `Shape.boundingBox()` in DSN units: `[min_x, min_y, max_x, max_y]`.
    ///
    /// The `PolygonPath` variant reproduces the upstream expression verbatim, brackets and all —
    /// `Math.max(bounds[2], coordinateArr[i]) + offset` adds the offset to the running maximum
    /// on every iteration rather than to the coordinate, so the x-maximum grows by
    /// `corner_count * offset`. It only feeds the board bounding box, which is inflated by 1000
    /// units anyway (`PolygonPath.java:106-124`).
    pub fn bounding_box(&self) -> [f64; 4] {
        match self {
            DsnShape::Rect { coor, .. } => *coor,
            DsnShape::Circle { coor, .. } => [
                coor[1] - coor[0],
                coor[2] - coor[0],
                coor[1] + coor[0],
                coor[2] + coor[0],
            ],
            DsnShape::Polygon { coor, .. } => {
                let mut bounds = [
                    f64::from(i32::MAX),
                    f64::from(i32::MAX),
                    f64::from(i32::MIN),
                    f64::from(i32::MIN),
                ];
                for (index, value) in coor.iter().enumerate() {
                    if index % 2 == 0 {
                        bounds[0] = bounds[0].min(*value);
                        bounds[2] = bounds[2].max(*value);
                    } else {
                        bounds[1] = bounds[1].min(*value);
                        bounds[3] = bounds[3].max(*value);
                    }
                }
                bounds
            }
            DsnShape::PolygonPath { width, coor, .. }
            | DsnShape::PolylinePath { width, coor, .. } => {
                let offset = width / 2.0;
                let mut bounds = [
                    f64::from(i32::MAX),
                    f64::from(i32::MAX),
                    f64::from(i32::MIN),
                    f64::from(i32::MIN),
                ];
                for (index, value) in coor.iter().enumerate() {
                    if index % 2 == 0 {
                        bounds[0] = bounds[0].min(*value - offset);
                        bounds[2] = bounds[2].max(*value) + offset;
                    } else {
                        bounds[1] = bounds[1].min(*value - offset);
                        bounds[3] = bounds[3].max(*value + offset);
                    }
                }
                bounds
            }
        }
    }
}

/// A shape after transformation into board coordinates.
///
/// The variants are the concrete `geometry.planar.Shape` implementations upstream's
/// `transformToBoard` can return. [`BoardShape::Polygon`] carries the corners already normalized
/// the way `PolygonShape`'s constructor normalizes them (counter-clockwise, duplicate and
/// collinear corners dropped, lowest-y-then-lowest-x corner first), because the convex hull walk
/// and the winding tests downstream assume it.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardShape {
    /// From `(rect …)`.
    Box(IntBox),
    /// From `(circle …)`. Upstream keeps a `Circle`, which is convex but **not** a
    /// `PolylineShape`, so a circular board outline or a circular area-with-holes is rejected
    /// downstream just as it is upstream.
    Circle(Circle),
    /// From `(polygon …)`, or from a zero-width `(path …)`.
    Polygon(Vec<IntPoint>),
    /// From a `(path …)` with at most two corners: the bounding octagon of the corners,
    /// enlarged by the half width.
    Octagon(IntOctagon),
    /// From a `(path …)` with three or more corners and a non-zero width: the bounding tile of
    /// the corner polygon, enlarged by the half width.
    Simplex(Simplex),
}

impl BoardShape {
    /// Upstream `Shape.dimension()`: 2 for an area, lower for degenerate geometry.
    pub fn dimension(&self) -> i32 {
        match self {
            BoardShape::Box(b) => b.dimension(),
            BoardShape::Circle(c) => c.dimension(),
            BoardShape::Octagon(o) => o.dimension(),
            BoardShape::Simplex(s) => s.dimension(),
            BoardShape::Polygon(corners) => polygon_dimension(corners),
        }
    }

    /// The smallest surrounding box.
    pub fn bounding_box(&self) -> IntBox {
        match self {
            BoardShape::Box(b) => *b,
            BoardShape::Circle(c) => IntBox::new(
                c.center.x - c.radius,
                c.center.y - c.radius,
                c.center.x + c.radius,
                c.center.y + c.radius,
            ),
            BoardShape::Octagon(o) => o.bounding_box(),
            BoardShape::Simplex(s) => s.bounding_box(),
            BoardShape::Polygon(corners) => bounding_box_of(corners),
        }
    }

    /// True for the shapes upstream's `PolylineShape` interface covers — the board outline and
    /// the boundary of an area with holes must be one of these.
    pub fn is_polyline_shape(&self) -> bool {
        !matches!(self, BoardShape::Circle(_))
    }

    /// The corners, for the shapes that have them.
    pub fn corners(&self) -> Option<&[IntPoint]> {
        match self {
            BoardShape::Polygon(corners) => Some(corners),
            _ => None,
        }
    }
}

/// Upstream `PolygonShape.dimension()`: `-1` when there are no corners, `0` for one, `1` for two
/// (or for an all-collinear corner list, which the normalizer collapses to two), else `2`.
fn polygon_dimension(corners: &[IntPoint]) -> i32 {
    match corners.len() {
        0 => -1,
        1 => 0,
        2 => 1,
        _ => 2,
    }
}

fn bounding_box_of(corners: &[IntPoint]) -> IntBox {
    if corners.is_empty() {
        return IntBox::new(0, 0, -1, -1);
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for corner in corners {
        min_x = min_x.min(corner.x);
        min_y = min_y.min(corner.y);
        max_x = max_x.max(corner.x);
        max_y = max_y.max(corner.y);
    }
    IntBox::new(min_x, min_y, max_x, max_y)
}

/// Clamps a coordinate value into the `[-CRIT_INT, CRIT_INT]` envelope.
pub fn clamp_board_coord(val: f64) -> f64 {
    if !val.is_finite() {
        0.0
    } else {
        val.clamp(-super::lex::CRIT_INT, super::lex::CRIT_INT)
    }
}

/// Upstream `PolygonShape`'s constructor normalization.
///
/// 1. Revert to counter-clockwise if the winding number says clockwise.
/// 2. Drop a repeated closing corner.
/// 3. Drop the last corner if it is collinear with its neighbour and the first; drop the first if
///    it is collinear with the second and the last.
/// 4. Rotate so the lowest-y (then lowest-x) corner comes first.
///
/// Step 4 is what makes the shape's corner list canonical, and the convex-hull walk in
/// [`convex_hull`] depends on step 1.
pub fn normalize_polygon_corners(corners: &[IntPoint]) -> Vec<IntPoint> {
    let limit = super::lex::CRIT_INT as i32;
    let clamped_corners: Vec<IntPoint> = corners
        .iter()
        .map(|p| IntPoint::new(p.x.clamp(-limit, limit), p.y.clamp(-limit, limit)))
        .collect();
    let points: Vec<Point> = clamped_corners.iter().map(|p| Point::Int(*p)).collect();
    let polygon = Polygon::new(&points);
    let polygon = if polygon.is_clockwise() == Some(true) {
        polygon.revert_corners()
    } else {
        polygon
    };
    let current: Vec<IntPoint> = polygon
        .corner_array()
        .iter()
        .filter_map(|p| match p {
            Point::Int(ip) => Some(*ip),
            Point::Rational(_) => None,
        })
        .collect();
    if current.is_empty() {
        return current;
    }
    let mut last = current.len() - 1;
    if last > 0 && current[0] == current[last] {
        last -= 1;
    }
    if last >= 2 && side_of(&current[last], &current[last - 1], &current[0]) == 0 {
        last -= 1;
    }
    let mut first = 0usize;
    if last >= 2 && side_of(&current[0], &current[1], &current[last]) == 0 {
        first += 1;
    }
    if first > last {
        return Vec::new();
    }
    let mut start = first;
    for index in (first + 1)..=last {
        let candidate = current[index];
        let best = current[start];
        if candidate.y < best.y || (candidate.y == best.y && candidate.x < best.x) {
            start = index;
        }
    }
    let mut result = Vec::with_capacity(last - first + 1);
    result.extend_from_slice(&current[start..=last]);
    result.extend_from_slice(&current[first..start]);
    result
}

/// Sign of the cross product `(to - from) × (point - from)`: positive when `point` is left of the
/// directed line `from → to`, `0` when collinear. Exact `i64` arithmetic — coordinates are bounded
/// by `CRIT_INT`, so no `f64` decides an orientation.
fn side_of(point: &IntPoint, from: &IntPoint, to: &IntPoint) -> i64 {
    let dx1 = (to.x as i128) - (from.x as i128);
    let dy1 = (to.y as i128) - (from.y as i128);
    let dx2 = (point.x as i128) - (from.x as i128);
    let dy2 = (point.y as i128) - (from.y as i128);
    (dx1 * dy2 - dy1 * dx2).signum() as i64
}

/// Upstream `PolygonShape.convexHull()` — repeatedly drop a corner that is not a left turn.
///
/// The input must be counter-clockwise ([`normalize_polygon_corners`] guarantees it). Upstream
/// recurses through a fresh `PolygonShape` each time, which re-normalizes; the loop here does the
/// same by re-normalizing after every removal.
pub fn convex_hull(corners: &[IntPoint]) -> Vec<IntPoint> {
    let mut current = corners.to_vec();
    loop {
        if current.len() <= 2 {
            return current;
        }
        let mut removed = None;
        let count = current.len();
        let mut prev = current[count - 1];
        let mut curr = current[0];
        for index in 0..count {
            let next = if index == count - 1 {
                current[0]
            } else {
                current[index + 1]
            };
            if side_of(&next, &prev, &curr) <= 0 {
                removed = Some(index);
                break;
            }
            prev = curr;
            curr = next;
        }
        match removed {
            None => return current,
            Some(index) => {
                current.remove(index);
                current = normalize_polygon_corners(&current);
            }
        }
    }
}

/// Upstream `PolygonShape.boundingTile()`: the convex hull's edges as a [`Simplex`].
pub fn bounding_tile(corners: &[IntPoint]) -> Simplex {
    let hull = convex_hull(corners);
    if hull.len() < 3 {
        return Simplex::get_instance(&[]);
    }
    let mut lines = Vec::with_capacity(hull.len());
    for index in 0..hull.len() {
        let a = hull[index];
        let b = hull[(index + 1) % hull.len()];
        lines.push(Line { a, b });
    }
    Simplex::get_instance(&lines)
}

/// Upstream `Simplex.simplify()`: collapse to the cheapest equivalent representation.
pub fn simplify(shape: BoardShape) -> BoardShape {
    match shape {
        BoardShape::Simplex(simplex) => {
            if simplex.is_int_box() {
                BoardShape::Box(simplex.bounding_box())
            } else if let Some(octagon) = simplex.to_int_octagon() {
                BoardShape::Octagon(octagon)
            } else {
                BoardShape::Simplex(simplex)
            }
        }
        other => other,
    }
}

/// Upstream `Shape.transformToBoard`: absolute coordinates, base offset applied.
pub fn transform_to_board(shape: &DsnShape, transform: &CoordinateTransform) -> Option<BoardShape> {
    match shape {
        DsnShape::Rect { coor, .. } => {
            let p1 = transform.dsn_to_board_point(coor[0].min(coor[2]), coor[1].min(coor[3]));
            let p2 = transform.dsn_to_board_point(coor[0].max(coor[2]), coor[1].max(coor[3]));
            let lower_left =
                FloatPoint::new(clamp_board_coord(p1.x), clamp_board_coord(p1.y)).jround();
            let upper_right =
                FloatPoint::new(clamp_board_coord(p2.x), clamp_board_coord(p2.y)).jround();
            Some(BoardShape::Box(IntBox::new(
                lower_left.x,
                lower_left.y,
                upper_right.x,
                upper_right.y,
            )))
        }
        DsnShape::Circle { coor, .. } => {
            let center_fp = transform.dsn_to_board_point(coor[1], coor[2]);
            let center = FloatPoint::new(
                clamp_board_coord(center_fp.x),
                clamp_board_coord(center_fp.y),
            )
            .jround();
            let radius = clamp_board_coord(transform.dsn_to_board(coor[0]) / 2.0).jround() as i32;
            Some(BoardShape::Circle(Circle::new(center, radius)))
        }
        DsnShape::Polygon { coor, .. } => {
            let corners = round_points(coor, |x, y| transform.dsn_to_board_point(x, y));
            Some(BoardShape::Polygon(normalize_polygon_corners(&corners)))
        }
        DsnShape::PolygonPath { width, coor, .. } => Some(path_to_board(
            *width, coor, transform, /* relative = */ false,
        )),
        // `PolylinePath.transformToBoard` logs "not implemented" and returns null upstream; the
        // wiring reader is the only consumer and it builds the lines itself.
        DsnShape::PolylinePath { .. } => None,
    }
}

/// Upstream `Shape.transformToBoardRel`: no base offset. Used for everything inside a padstack or
/// a package, which is stored relative to the pin or component origin.
pub fn transform_to_board_rel(
    shape: &DsnShape,
    transform: &CoordinateTransform,
) -> Option<BoardShape> {
    match shape {
        DsnShape::Rect { coor, .. } => {
            let values: Vec<i32> = coor
                .iter()
                .map(|v| clamp_board_coord(transform.dsn_to_board(*v)).jround() as i32)
                .collect();
            // Upstream orders by the y coordinates only, so a rectangle given as
            // (upper-left, lower-right) is repaired but an x-inverted one is not.
            let result = if values[1] <= values[3] {
                IntBox::new(values[0], values[1], values[2], values[3])
            } else {
                IntBox::new(values[0], values[3], values[2], values[1])
            };
            Some(BoardShape::Box(result))
        }
        DsnShape::Circle { coor, .. } => {
            let radius = clamp_board_coord(transform.dsn_to_board(coor[0]) / 2.0).jround() as i32;
            let cx = clamp_board_coord(transform.dsn_to_board(coor[1])).jround() as i32;
            let cy = clamp_board_coord(transform.dsn_to_board(coor[2])).jround() as i32;
            let center = IntPoint::new(cx, cy);
            Some(BoardShape::Circle(Circle::new(center, radius)))
        }
        DsnShape::Polygon { coor, .. } => {
            if coor.len() < 2 {
                return Some(BoardShape::Simplex(Simplex::get_instance(&[])));
            }
            let corners = round_points(coor, |x, y| {
                fr_geometry::planar::FloatPoint::new(
                    transform.dsn_to_board(x),
                    transform.dsn_to_board(y),
                )
            });
            Some(BoardShape::Polygon(normalize_polygon_corners(&corners)))
        }
        DsnShape::PolygonPath { width, coor, .. } => Some(path_to_board(
            *width, coor, transform, /* relative = */ true,
        )),
        DsnShape::PolylinePath { .. } => None,
    }
}

/// Upstream `PolygonPath.transformToBoard[Rel]`.
///
/// The two-corner case is the standard oval-pad encoding and becomes a **bounding octagon
/// enlarged by the half width** — an octagonal stadium, not a polyline. Treating `(path …)`
/// inside `(shape …)` as a zero-width polyline instead produces a degenerate shape and then a
/// one-unit pad via the rescue in `build.rs` (`dsn-board-semantics.md` §14.3).
fn path_to_board(
    width: f64,
    coor: &[f64],
    transform: &CoordinateTransform,
    relative: bool,
) -> BoardShape {
    let corners: Vec<fr_geometry::planar::FloatPoint> = coor
        .chunks_exact(2)
        .map(|pair| {
            let pt = if relative {
                fr_geometry::planar::FloatPoint::new(
                    transform.dsn_to_board(pair[0]),
                    transform.dsn_to_board(pair[1]),
                )
            } else {
                transform.dsn_to_board_point(pair[0], pair[1])
            };
            fr_geometry::planar::FloatPoint::new(clamp_board_coord(pt.x), clamp_board_coord(pt.y))
        })
        .collect();
    if corners.is_empty() {
        return BoardShape::Octagon(IntOctagon::EMPTY);
    }
    let offset = clamp_board_coord(transform.dsn_to_board(width) / 2.0).max(0.0);
    if corners.len() <= 2 {
        let octagon = fr_geometry::planar::FloatPoint::bounding_octagon(&corners);
        if offset > 0.0 {
            return BoardShape::Octagon(octagon.offset(offset));
        } else {
            return BoardShape::Octagon(octagon);
        }
    }
    let rounded: Vec<IntPoint> = corners.iter().map(|p| p.jround()).collect();
    let normalized = normalize_polygon_corners(&rounded);
    if normalized.len() < 3 {
        let octagon = fr_geometry::planar::FloatPoint::bounding_octagon(&corners);
        if offset > 0.0 {
            return BoardShape::Octagon(octagon.offset(offset));
        } else {
            return BoardShape::Octagon(octagon);
        }
    }
    if offset > 0.0 {
        BoardShape::Simplex(bounding_tile(&normalized).enlarge(offset))
    } else {
        BoardShape::Polygon(normalized)
    }
}

fn round_points<F>(coor: &[f64], map: F) -> Vec<IntPoint>
where
    F: Fn(f64, f64) -> fr_geometry::planar::FloatPoint,
{
    coor.chunks_exact(2)
        .map(|pair| {
            let pt = map(pair[0], pair[1]);
            FloatPoint::new(clamp_board_coord(pt.x), clamp_board_coord(pt.y)).jround()
        })
        .collect()
}

/// Translates a board shape — used for the outline holes and for placed package geometry.
pub fn translate_board_shape(shape: &BoardShape, vector: &Vector) -> BoardShape {
    match shape {
        BoardShape::Box(b) => BoardShape::Box(b.translate_by(vector)),
        BoardShape::Octagon(o) => BoardShape::Octagon(o.translate_by(vector)),
        BoardShape::Simplex(s) => BoardShape::Simplex(s.translate_by(vector)),
        BoardShape::Circle(c) => {
            let moved = Point::Int(c.center).translate_by(vector);
            let center = match moved {
                Point::Int(p) => p,
                Point::Rational(r) => r.to_float().jround(),
            };
            BoardShape::Circle(Circle::new(center, c.radius))
        }
        BoardShape::Polygon(corners) => BoardShape::Polygon(
            corners
                .iter()
                .map(|p| match p.translate_by(vector) {
                    Point::Int(q) => q,
                    Point::Rational(r) => r.to_float().jround(),
                })
                .collect(),
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------------------------

/// One entry of a `(rule …)` scope.
#[derive(Debug, Clone, PartialEq)]
pub enum DsnRule {
    /// `(width w)`.
    Width(f64),
    /// `(clearance v [(type a-b c_d …)])`.
    Clearance {
        /// Clearance value in DSN units.
        value: f64,
        /// The `(type …)` list as `nextStringList('-')` produced it: `a-b` arrives as **two**
        /// entries and `a_b` as one, and `Structure.setClearanceRule` branches on that length.
        class_pairs: Vec<String>,
    },
}

/// `(layer_rule L1 L2 (rule …))`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnLayerRule {
    /// The layer names the rules apply to.
    pub layer_names: Vec<String>,
    /// The rules themselves.
    pub rules: Vec<DsnRule>,
}

// ---------------------------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------------------------

/// `(padstack <name> (shape …)* (attach on|off) (absolute on|off))`.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPadstack {
    /// Name with any `.<digits>` suffix already stripped, as upstream does at the definition site.
    pub name: String,
    /// One shape scope per copper layer the padstack covers.
    pub shapes: Vec<DsnShape>,
    /// `(attach off)` clears this; it is upstream's `isDrillable` / `attachAllowed`.
    pub attach_allowed: bool,
    /// `(absolute on)` sets this; it disables the back-side layer flip.
    pub placed_absolute: bool,
}

/// One `(pin <padstack> [(rotate d)] <name> x y [(rotate d)])` record.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPin {
    /// Padstack reference, before `.N` stripping.
    pub padstack_name: String,
    /// Pin name; numeric names arrive here as text because the reader forces `NAME` twice.
    pub pin_name: String,
    /// Pin offset in DSN units, relative to the component origin.
    pub rel_coor: [f64; 2],
    /// Pin rotation in degrees.
    pub rotation: f64,
}

/// An area scope: a border shape plus window shapes, an optional name and an optional clearance
/// class. Upstream `Shape.ReadAreaScopeResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnArea {
    /// The area name, `None` when the leading token was absent or empty.
    pub name: Option<String>,
    /// Border first, then one entry per `(window …)`.
    pub shapes: Vec<DsnShape>,
    /// `(clearance_class …)` inside the area scope.
    pub clearance_class_name: Option<String>,
}

/// `(image <name> (side …) (pin …)* (outline …)* (keepout …)*)`.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPackage {
    /// Footprint name.
    pub name: String,
    /// Pins in file order; the index is the board-side pin number.
    pub pins: Vec<DsnPin>,
    /// Outline shapes in file order.
    pub outlines: Vec<DsnShape>,
    /// Wire keepouts.
    pub keepouts: Vec<DsnArea>,
    /// Via keepouts.
    pub via_keepouts: Vec<DsnArea>,
    /// Placement keepouts.
    pub place_keepouts: Vec<DsnArea>,
    /// `(side back)` clears this; **any** other token counts as front, typos included.
    pub is_front: bool,
}

// ---------------------------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------------------------

/// A `(pin|keepout|via_keepout|place_keepout <name> (clearance_class <n>))` override inside a
/// `(place …)` record.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemClearanceInfo {
    /// The item name the override applies to.
    pub name: String,
    /// The clearance class name.
    pub clearance_class: String,
}

/// One `(place <name> x y front|back <rotation> …)` record.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentLocation {
    /// Reference designator.
    pub name: String,
    /// Location in DSN units, or `None` for a component that is "not yet placed" — upstream
    /// creates the component but inserts no board items for it.
    pub coor: Option<[f64; 2]>,
    /// False only for the literal token `back`.
    pub is_front: bool,
    /// Rotation in degrees; may be fractional.
    pub rotation: f64,
    /// `(lock_type … position …)`; makes every item of the component `SYSTEM_FIXED`.
    pub position_fixed: bool,
    /// Per-pin clearance-class overrides, keyed by pin name.
    pub pin_infos: Vec<ItemClearanceInfo>,
    /// Per-keepout clearance-class overrides.
    pub keepout_infos: Vec<ItemClearanceInfo>,
    /// Per-via-keepout clearance-class overrides.
    pub via_keepout_infos: Vec<ItemClearanceInfo>,
    /// Per-place-keepout clearance-class overrides.
    pub place_keepout_infos: Vec<ItemClearanceInfo>,
    /// `(PN "…")`, which arrives as a *string* because the lexer has no `PN` keyword.
    pub part_number: Option<String>,
}

/// All `(place …)` records under one `(component <libname> …)` scope.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentPlacement {
    /// The library (footprint) name.
    pub lib_name: String,
    /// The placements.
    pub locations: Vec<ComponentLocation>,
}

// ---------------------------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------------------------

/// A `(component)-(pin)` reference inside `(pins …)` / `(order …)` / `(fromto …)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetPin {
    /// Component reference designator.
    pub component_name: String,
    /// Pin name.
    pub pin_name: String,
}

/// One `(net <name> [subnet] …)` scope.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnNet {
    /// Net name.
    pub name: String,
    /// Explicit subnet number, defaulting to 1.
    pub subnet_number: i32,
    /// True when the scope was `(net X)` with nothing inside.
    pub scope_is_empty: bool,
    /// Pins from `(pins …)` or `(order …)`.
    pub pins: Vec<NetPin>,
    /// True when the pins came from `(order …)`, which splits them into consecutive pairs.
    pub pin_order_found: bool,
    /// One entry per `(fromto …)`; each is its own subnet.
    pub fromto: Vec<Vec<NetPin>>,
    /// `(rule (width w))` on the net.
    pub rules: Vec<DsnRule>,
}

/// One `(class <name> <nets…> …)` scope.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnNetClass {
    /// Class name.
    pub name: String,
    /// Member net names.
    pub net_list: Vec<String>,
    /// `(rule …)` entries.
    pub rules: Vec<DsnRule>,
    /// `(layer_rule …)` entries.
    pub layer_rules: Vec<DsnLayerRule>,
    /// `(circuit (use_via …))`.
    pub use_via: Vec<String>,
    /// `(circuit (use_layer …))`.
    pub use_layer: Vec<String>,
    /// `(via_rule …)`.
    pub via_rule: Option<String>,
    /// `(clearance_class …)`.
    pub trace_clearance_class: Option<String>,
    /// `(pull_tight on|off)`, default on.
    pub pull_tight: bool,
    /// `(shove_fixed on|off)`, default off.
    pub shove_fixed: bool,
    /// `(circuit (length max min))` minimum.
    pub min_trace_length: f64,
    /// `(circuit (length max min))` maximum.
    pub max_trace_length: f64,
}

/// One `(class_class (classes …) (rule …))` scope.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnClassClass {
    /// The class names listed in `(classes …)`.
    pub class_names: Vec<String>,
    /// `(rule …)` entries.
    pub rules: Vec<DsnRule>,
    /// `(layer_rule …)` entries.
    pub layer_rules: Vec<DsnLayerRule>,
}

/// One `(via <viaName> <padstackName> <clearanceClassName> [attach])` info scope.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnViaInfo {
    /// The via info's own name.
    pub name: String,
    /// The padstack it refers to. The `.N` cleanup is **not** applied here upstream.
    pub padstack_name: String,
    /// The clearance class name; an unknown one falls back to class 1.
    pub clearance_class_name: String,
    /// The trailing `attach` keyword.
    pub attach_allowed: bool,
}

// ---------------------------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------------------------

/// A net reference inside a `(wire …)` or `(via …)`: `(net <name> [subnet])`.
///
/// A subnet of `0` means "no subnet given", which resolves to **all** subnets of that name.
#[derive(Debug, Clone, PartialEq)]
pub struct NetRef {
    /// Net name.
    pub name: String,
    /// Subnet number, `0` when absent.
    pub subnet_number: i32,
}

/// The fixed states `Wiring.calcFixed` maps `(type …)` onto.
///
/// `protect` and every unrecognized token land on `UserFixed`; only the literal `normal` is
/// unfixed. `(type route)` — which KiCad writes for ordinary routed copper — is *not* one of the
/// three recognized tokens and therefore imports as `UserFixed`, which is why re-imported routes
/// are neither shoved nor ripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsnFixedState {
    /// `(type normal)` or no `(type …)` at all.
    Unfixed,
    /// `(type shove_fixed)`.
    ShoveFixed,
    /// `(type protect)`, `(type route)`, anything unrecognized.
    UserFixed,
    /// `(type fix)`.
    SystemFixed,
}

/// One `(wire …)` record.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnWire {
    /// The `(path …)` or `(polyline_path …)` that makes this a trace, if any.
    pub path: Option<DsnShape>,
    /// The `(rect|polygon|circle …)` that makes this a conduction area, if any.
    pub border_shape: Option<DsnShape>,
    /// `(window …)` holes of a conduction area.
    pub windows: Vec<DsnShape>,
    /// `(net …)`.
    pub net: Option<NetRef>,
    /// `(clearance_class …)`.
    pub clearance_class_name: Option<String>,
    /// `(type …)`.
    pub fixed: DsnFixedState,
}

/// One `(via <padstack> x y …)` record in the wiring scope.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnWiringVia {
    /// Padstack name, before `.N` stripping.
    pub padstack_name: String,
    /// Location in DSN units.
    pub location: [f64; 2],
    /// `(net …)`.
    pub net: Option<NetRef>,
    /// `(clearance_class …)`.
    pub clearance_class_name: Option<String>,
    /// `(type …)`.
    pub fixed: DsnFixedState,
}

// ---------------------------------------------------------------------------------------------
// The whole file
// ---------------------------------------------------------------------------------------------

/// A `(plane <netName> <shape> (window …)*)` record, buffered until the end of the structure
/// scope because nets do not exist yet while the structure scope is being read.
#[derive(Debug, Clone, PartialEq)]
pub struct DsnPlane {
    /// The net the pour belongs to; always subnet 1.
    pub net_name: String,
    /// Border plus windows.
    pub area: DsnArea,
}

/// Per-layer routing settings from `(autoroute_settings (layer_rule …))`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnLayerSetting {
    /// Layer name.
    pub layer_name: String,
    /// The layer index [`layer_name`](Self::layer_name) resolved to, through
    /// `LayerStructure.getNo` and therefore through the Electra `Top`/`Bottom` fallback.
    ///
    /// Always in range: upstream's `AutorouteSettings.read_layer_rule` returns `null` when the
    /// name resolves to `-1`, which makes `read_scope` return `null` too and drops the *whole*
    /// `autoroute_settings` scope (`AutorouteSettings.java:82-85`, `:52-55`). The reader does
    /// the same, so an out-of-range index can never reach the AST.
    pub layer_no: usize,
    /// `(active on|off)`.
    pub active: Option<bool>,
    /// `(preferred_direction horizontal|vertical)`.
    pub preferred_direction_is_horizontal: Option<bool>,
    /// `(preferred_direction_trace_costs …)`.
    pub preferred_direction_trace_costs: Option<f64>,
    /// `(against_preferred_direction_trace_costs …)`.
    pub against_preferred_direction_trace_costs: Option<f64>,
}

/// `(autoroute_settings …)`, when it was parsed at all — see the guard bug in
/// [`crate::specctra::read`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnAutorouteSettings {
    /// `(autoroute on|off)`.
    pub run_router: Option<bool>,
    /// `(postroute on|off)`.
    pub run_optimizer: Option<bool>,
    /// `(vias on|off)`.
    pub vias_allowed: Option<bool>,
    /// `(via_costs n)`.
    pub via_costs: Option<i32>,
    /// `(plane_via_costs n)`.
    pub plane_via_costs: Option<i32>,
    /// `(start_ripup_costs n)`.
    pub start_ripup_costs: Option<i32>,
    /// Per-layer rules.
    pub layer_settings: Vec<DsnLayerSetting>,
}

/// The `(structure …)` scope.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnStructure {
    /// The physical layers, in the order they survived `(type …)` validation.
    pub layers: Vec<DsnLayer>,
    /// The first `(boundary <shape>)` whose layer is `pcb`; the board bounding box.
    pub bounding_shape: Option<DsnShape>,
    /// Every other boundary shape.
    pub outline_shapes: Vec<DsnShape>,
    /// `(boundary … (clearance_class <n>))`.
    pub outline_clearance_class_name: Option<String>,
    /// PCB-level `(rule …)` entries.
    pub default_rules: Vec<DsnRule>,
    /// `(layer L (rule …))` entries, resolved to layer indices at build time.
    pub layer_dependent_rules: Vec<DsnLayerRule>,
    /// `(via …)` padstack names, spares appended at the end.
    pub via_padstack_names: Vec<String>,
    /// Wire keepouts.
    pub keepouts: Vec<DsnArea>,
    /// Via keepouts.
    pub via_keepouts: Vec<DsnArea>,
    /// Placement keepouts.
    pub place_keepouts: Vec<DsnArea>,
    /// Power planes.
    pub planes: Vec<DsnPlane>,
    /// `(control (via_at_smd on|off))`.
    pub via_at_smd_allowed: bool,
    /// A bare `(flip_style rotate_first)` in the structure scope — Electra puts it here.
    pub flip_style_rotate_first: bool,
    /// `(snap_angle …)`, `None` when absent (the default is 45°).
    pub snap_angle: Option<i32>,
    /// `(autoroute_settings …)`, when the guard let it through.
    pub autoroute_settings: Option<DsnAutorouteSettings>,
}

/// The complete parsed file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DsnAst {
    /// The `(pcb <name>)` header token.
    pub design_name: String,
    /// `(string_quote …)`, default `"`.
    pub string_quote: String,
    /// `(host_cad …)`.
    pub host_cad: Option<String>,
    /// `(host_version …)`.
    pub host_version: Option<String>,
    /// `(constant a b)` pairs.
    pub constants: Vec<(String, String)>,
    /// Cleared by `(generated_by_freeroute)`.
    pub generated_by_host: bool,
    /// `(resolution <unit> <n>)` unit, defaulting to mil.
    pub unit: Unit,
    /// `(resolution <unit> <n>)` value, defaulting to 100.
    pub resolution: i32,
    /// The structure scope.
    pub structure: DsnStructure,
    /// Padstack definitions in file order; first definition wins on a duplicate name.
    pub padstacks: Vec<DsnPadstack>,
    /// Package definitions in file order.
    pub packages: Vec<DsnPackage>,
    /// Component placements, buffered until the end of the network scope.
    pub placements: Vec<ComponentPlacement>,
    /// `(net …)` scopes.
    pub nets: Vec<DsnNet>,
    /// `(class …)` scopes.
    pub net_classes: Vec<DsnNetClass>,
    /// `(class_class …)` scopes.
    pub class_classes: Vec<DsnClassClass>,
    /// `(via …)` info scopes in the network scope.
    pub via_infos: Vec<DsnViaInfo>,
    /// `(via_rule <name> <via>…)` scopes; the first element of each list is the rule name.
    pub via_rules: Vec<Vec<String>>,
    /// `(wire …)` records.
    pub wires: Vec<DsnWire>,
    /// `(via …)` records in the wiring scope.
    pub wiring_vias: Vec<DsnWiringVia>,
    /// True when a `(wiring …)` scope was present at all.
    pub has_wiring_scope: bool,
}

/// The DSN length units, with their micrometre factors
/// (`board/model/structure/Unit.java:7-10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unit {
    /// 25.4 µm.
    #[default]
    Mil,
    /// 25400 µm.
    Inch,
    /// 1000 µm.
    Mm,
    /// 1 µm.
    Um,
}

impl Unit {
    /// Upstream `Unit.fromString` = `Unit.valueOf(s.toUpperCase())`, i.e. case-insensitive and
    /// failing on anything else.
    ///
    /// Not `FromStr`: the upstream contract is "unknown unit ⇒ the resolution scope fails", not
    /// an error type, and the importer branches on the `Option`.
    pub fn from_dsn(text: &str) -> Option<Unit> {
        match text.to_ascii_lowercase().as_str() {
            "mil" => Some(Unit::Mil),
            "inch" => Some(Unit::Inch),
            "mm" => Some(Unit::Mm),
            "um" => Some(Unit::Um),
            _ => None,
        }
    }

    /// The unit's size in micrometres.
    pub fn micrometres(self) -> f64 {
        match self {
            Unit::Mil => 25.4,
            Unit::Inch => 25400.0,
            Unit::Mm => 1000.0,
            Unit::Um => 1.0,
        }
    }

    /// Upstream `Unit.scale(v, from, to) = v * from.µm / to.µm`.
    pub fn scale(value: f64, from: Unit, to: Unit) -> f64 {
        value * from.micrometres() / to.micrometres()
    }

    /// The spelling used in DSN and SES output.
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Mil => "mil",
            Unit::Inch => "inch",
            Unit::Mm => "mm",
            Unit::Um => "um",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_normalization_orients_counter_clockwise_and_rotates_to_lowest_corner() {
        // A clockwise square, given from the top-left corner.
        let clockwise = [
            IntPoint::new(0, 10),
            IntPoint::new(10, 10),
            IntPoint::new(10, 0),
            IntPoint::new(0, 0),
        ];
        let normalized = normalize_polygon_corners(&clockwise);
        assert_eq!(normalized.len(), 4);
        assert_eq!(normalized[0], IntPoint::new(0, 0));
        // Counter-clockwise from (0,0): right, up, left.
        assert_eq!(normalized[1], IntPoint::new(10, 0));
        assert_eq!(normalized[2], IntPoint::new(10, 10));
        assert_eq!(normalized[3], IntPoint::new(0, 10));
    }

    #[test]
    fn convex_hull_drops_a_reflex_corner() {
        // An L shape: the reflex corner at (5,5) must go.
        let corners = normalize_polygon_corners(&[
            IntPoint::new(0, 0),
            IntPoint::new(10, 0),
            IntPoint::new(10, 5),
            IntPoint::new(5, 5),
            IntPoint::new(5, 10),
            IntPoint::new(0, 10),
        ]);
        let hull = convex_hull(&corners);
        assert!(!hull.contains(&IntPoint::new(5, 5)));
        assert!(hull.contains(&IntPoint::new(0, 0)));
        assert!(hull.contains(&IntPoint::new(10, 0)));
    }

    #[test]
    fn unit_factors_match_upstream() {
        assert_eq!(Unit::from_dsn("UM"), Some(Unit::Um));
        assert_eq!(Unit::from_dsn("nm"), None);
        assert!((Unit::scale(1.0, Unit::Um, Unit::Mm) - 0.001).abs() < 1e-12);
    }
}
