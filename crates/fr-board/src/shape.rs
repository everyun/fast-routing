//! The tile shapes the search trees index, and the padstack geometry drill
//! items derive theirs from.
//!
//! Upstream's `TileShape` is an abstract class with three concrete subclasses
//! (`IntBox`, `IntOctagon`, `Simplex`) dispatching through a visitor;
//! `ARCHITECTURE.md` calls for `TileShape = enum{Box, Oct, Simplex}` in
//! `fr-geometry`. Until that type lands there, [`TileShape`] here is that enum,
//! kept deliberately thin — it owns no geometry of its own, only the dispatch
//! and the one derived quantity the tree needs ([`bounds`](TileShape::bounds)).
//! When `fr-geometry` grows the type this module keeps the padstack table and
//! re-exports the geometry one.
//!
//! # Why the bounds are always octagonal
//!
//! Upstream has two `ShapeBoundingDirections` singletons and picks `IntBox`
//! bounds only for the 90°-specialized tree (`search-tree-obstacles.md` §1.2).
//! `fr-datastructures`' arena tree stores `OctBounds` for every node and treats
//! a box as the degenerate octagon, which `search-tree-obstacles.md` §10
//! ("Restructure") explicitly endorses: *"`IntBox` trees can use the same
//! 8-int layout (box = degenerate octagon) to keep one code path, as Java's 45°
//! default tree effectively does."* So the bounding-direction choice disappears
//! and only the *exact* shape flavor is still per-tree.

use fr_datastructures::arena_shape_tree::OctBounds;
use fr_geometry::planar::{IntBox, IntOctagon, IntPoint, IntVector, Point, Simplex, Vector};

/// One convex tile shape as stored at a search-tree leaf.
///
/// The three flavors are upstream's, and the ordering of the `match` arms in
/// [`intersects`](TileShape::intersects) is the whole point of keeping them
/// apart: a box/box or octagon/octagon test is a handful of integer compares,
/// while a simplex test builds an intersection.
#[derive(Debug, Clone, PartialEq)]
pub enum TileShape {
    /// Axis-parallel rectangle.
    Box(IntBox),
    /// 45°-bounded octagon.
    Oct(IntOctagon),
    /// Intersection of half planes — the general case, and what a trace segment
    /// offset shape always is.
    Simplex(Simplex),
}

impl TileShape {
    /// The degenerate tile at a point, upstream's `TileShape.getInstance(point)`
    /// as used by `pickItems` and by the contact predicate
    /// (`drc-connectivity.md` §4.3).
    pub fn from_point(point: &Point) -> TileShape {
        TileShape::Oct(point.surrounding_octagon())
    }

    /// True when the shape encloses nothing.
    pub fn is_empty(&self) -> bool {
        match self {
            TileShape::Box(b) => b.is_empty(),
            TileShape::Oct(o) => o.is_empty(),
            TileShape::Simplex(s) => s.is_empty(),
        }
    }

    /// 2 for an area, 1 for a segment, 0 for a point, −1 for empty.
    pub fn dimension(&self) -> i32 {
        match self {
            TileShape::Box(b) => b.dimension(),
            TileShape::Oct(o) => o.dimension(),
            TileShape::Simplex(s) => s.dimension(),
        }
    }

    /// Smallest enclosing axis-parallel box.
    pub fn bounding_box(&self) -> IntBox {
        match self {
            TileShape::Box(b) => *b,
            TileShape::Oct(o) => o.bounding_box(),
            TileShape::Simplex(s) => s.bounding_box(),
        }
    }

    /// Outward bounding box when the shape is non-empty and bounded; `None`
    /// for empty or unbounded shapes where a bounding-box reject would not be
    /// sound.
    pub fn bounded_bounding_box(&self) -> Option<IntBox> {
        match self {
            TileShape::Box(b) => (!b.is_empty()).then_some(*b),
            TileShape::Oct(o) => (!o.is_empty()).then(|| o.bounding_box()),
            TileShape::Simplex(s) => s.bounded_bounding_box(),
        }
    }

    /// The regular bounding shape stored at the leaf
    /// (`search-tree-obstacles.md` §1.1: `Leaf.bounding_shape =
    /// object_shape.bounding_shape(bounding_directions)`).
    ///
    /// An unbounded simplex — which upstream logs a warning for and refuses to
    /// query with — yields the whole coordinate envelope, so that such a shape
    /// is over-reported by the broad phase rather than silently dropped by it.
    pub fn bounds(&self) -> OctBounds {
        match self {
            TileShape::Box(b) => {
                OctBounds::from_box(b.left_x(), b.bottom_y(), b.right_x(), b.top_y())
            }
            TileShape::Oct(o) => octagon_bounds(o),
            TileShape::Simplex(s) => match s.bounding_octagon() {
                Some(o) if s.is_bounded() => octagon_bounds(&o),
                _ => UNBOUNDED,
            },
        }
    }

    /// Exact intersection test, with the cheap flavor pairs short-circuited.
    ///
    /// Upstream additionally skips the exact test entirely when both shapes are
    /// octagons in a 45°-bounded tree, because the bounding-shape test already
    /// decided it (`search-tree-obstacles.md` §2.2, "fast path"). That is an
    /// optimization of the *caller*, not a different answer: an octagon's
    /// octagonal bounding shape is the octagon itself, so the two tests agree.
    /// It is spelled out here as the `Oct`/`Oct` arm.
    pub fn intersects(&self, other: &TileShape) -> bool {
        match (self, other) {
            (TileShape::Box(a), TileShape::Box(b)) => a.intersects(b),
            (TileShape::Oct(a), TileShape::Oct(b)) => a.intersects(b),
            (TileShape::Box(a), TileShape::Oct(b)) | (TileShape::Oct(b), TileShape::Box(a)) => {
                a.to_int_octagon().intersects(b)
            }
            (TileShape::Simplex(a), TileShape::Box(b))
            | (TileShape::Box(b), TileShape::Simplex(a)) => a.intersects_box(b),
            (TileShape::Simplex(a), TileShape::Oct(b))
            | (TileShape::Oct(b), TileShape::Simplex(a)) => a.intersects_octagon(b),
            (TileShape::Simplex(a), TileShape::Simplex(b)) => a.intersects(b),
        }
    }

    /// True when `point` is inside the shape or on its border.
    pub fn contains(&self, point: &Point) -> bool {
        match self {
            TileShape::Box(b) => match point {
                Point::Int(p) => p.is_contained_in(b),
                Point::Rational(_) => point.is_contained_in(b),
            },
            TileShape::Oct(o) => o.to_simplex().contains(point),
            TileShape::Simplex(s) => s.contains(point),
        }
    }

    /// Grows the shape by `distance`, keeping its flavor.
    ///
    /// The regular flavors use `offset`, the exact Minkowski-style growth of
    /// upstream's `ConvexShape.offset`; a simplex uses `Simplex.enlarge`, which
    /// translates every border line outward and clips against the enlarged
    /// bounding octagon (`search-tree-obstacles.md` §2.3).
    pub fn enlarge(&self, distance: i32) -> TileShape {
        if distance == 0 {
            return self.clone();
        }
        let distance = distance as f64;
        match self {
            TileShape::Box(b) => TileShape::Box(b.offset(distance)),
            TileShape::Oct(o) => TileShape::Oct(o.offset(distance)),
            TileShape::Simplex(s) => TileShape::Simplex(s.enlarge(distance)),
        }
    }

    /// Translates the shape.
    pub fn translate_by(&self, vector: &Vector) -> TileShape {
        match self {
            TileShape::Box(b) => TileShape::Box(b.translate_by(vector)),
            TileShape::Oct(o) => TileShape::Oct(o.translate_by(vector)),
            TileShape::Simplex(s) => TileShape::Simplex(s.translate_by(vector)),
        }
    }

    /// Rotates the shape by `factor` × 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> TileShape {
        match self {
            TileShape::Box(b) => TileShape::Box(b.turn_90_degree(factor, pole)),
            // `IntOctagon` has no 90° rotation of its own; going through the
            // simplex and back keeps the flavor without inventing arithmetic.
            TileShape::Oct(o) => TileShape::Simplex(o.to_simplex().turn_90_degree(factor, pole))
                .to_flavor(AngleRestriction::FortyFive),
            TileShape::Simplex(s) => TileShape::Simplex(s.turn_90_degree(factor, pole)),
        }
    }

    /// Mirrors the shape at the vertical line through `pole` — the back-side
    /// placement transform.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> TileShape {
        match self {
            TileShape::Box(b) => TileShape::Simplex(b.to_simplex().mirror_vertical(pole))
                .to_flavor(AngleRestriction::Ninety),
            TileShape::Oct(o) => TileShape::Simplex(o.to_simplex().mirror_vertical(pole))
                .to_flavor(AngleRestriction::FortyFive),
            TileShape::Simplex(s) => TileShape::Simplex(s.mirror_vertical(pole)),
        }
    }

    /// Converts to a [`Simplex`].
    pub fn to_simplex(&self) -> Simplex {
        match self {
            TileShape::Box(b) => b.to_simplex(),
            TileShape::Oct(o) => o.to_simplex(),
            TileShape::Simplex(s) => s.clone(),
        }
    }

    /// Converts to the coarser flavor the board's angle restriction calls for
    /// (`search-tree-obstacles.md` §3.4).
    ///
    /// * [`Ninety`](AngleRestriction::Ninety) → the bounding box.
    /// * [`FortyFive`](AngleRestriction::FortyFive) → the bounding octagon,
    ///   **unless that octagon is really a box**, in which case the box is
    ///   kept: upstream does this "to avoid small corner cutoffs when taking the
    ///   offset as an octagon" (`ShapeSearchTree45Degree.java:439-445`), because
    ///   offsetting a true octagon chamfers its corners while offsetting a box
    ///   does not.
    /// * [`None`](AngleRestriction::None) → unchanged (upstream's exact
    ///   `bounding_tile`).
    pub fn to_flavor(&self, restriction: AngleRestriction) -> TileShape {
        match restriction {
            AngleRestriction::None => self.clone(),
            AngleRestriction::Ninety => TileShape::Box(self.bounding_box()),
            AngleRestriction::FortyFive => {
                let octagon = match self {
                    TileShape::Box(b) => return TileShape::Box(*b),
                    TileShape::Oct(o) => *o,
                    TileShape::Simplex(s) => match s.bounding_octagon() {
                        Some(o) => o,
                        None => return self.clone(),
                    },
                };
                let as_box = octagon.bounding_box();
                if as_box.to_int_octagon() == octagon.normalize() {
                    TileShape::Box(as_box)
                } else {
                    TileShape::Oct(octagon)
                }
            }
        }
    }

    /// Splits the shape into sections no wider than `max_section_width`
    /// (`search-tree-obstacles.md` §3.5).
    ///
    /// A huge copper pour indexed as one leaf destroys tree selectivity, so
    /// upstream grids it. Sections of dimension < 2 are dropped, and a shape
    /// that already fits comes back as itself.
    pub fn divide_into_sections(&self, max_section_width: f64) -> Vec<TileShape> {
        let bounding = self.bounding_box();
        if bounding.is_empty()
            || (bounding.width() as f64) <= max_section_width
                && (bounding.height() as f64) <= max_section_width
        {
            return vec![self.clone()];
        }
        match self {
            TileShape::Box(b) => b
                .divide_into_sections(max_section_width)
                .into_iter()
                .filter(|section| section.dimension() == 2)
                .map(TileShape::Box)
                .collect(),
            TileShape::Oct(o) => o
                .to_simplex()
                .divide_into_sections(max_section_width)
                .into_iter()
                .map(TileShape::Simplex)
                .collect(),
            TileShape::Simplex(s) => s
                .divide_into_sections(max_section_width)
                .into_iter()
                .map(TileShape::Simplex)
                .collect(),
        }
    }
}

/// The board-wide trace angle restriction, which decides the tile-shape flavor
/// of drill items and areas (`search-tree-obstacles.md` §3.4-3.5).
///
/// Upstream reads it from `rules.get_trace_angle_restriction()`, *not* from the
/// tree subclass — so the default tree stores octagonal pad shapes on a 45°
/// board even though it is not the 45° tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AngleRestriction {
    /// Any angle: shapes are kept exactly.
    None,
    /// 45° board — the freerouting default.
    #[default]
    FortyFive,
    /// 90° board: everything collapses to boxes.
    Ninety,
}

/// The full coordinate envelope, used as the bounds of an unbounded shape.
const UNBOUNDED: OctBounds = OctBounds::from_box(
    -fr_geometry::planar::Limits::CRIT_INT,
    -fr_geometry::planar::Limits::CRIT_INT,
    fr_geometry::planar::Limits::CRIT_INT,
    fr_geometry::planar::Limits::CRIT_INT,
);

fn octagon_bounds(octagon: &IntOctagon) -> OctBounds {
    OctBounds::new(
        octagon.left_x,
        octagon.bottom_y,
        octagon.right_x,
        octagon.top_y,
        octagon.upper_left_diagonal_x,
        octagon.lower_right_diagonal_x,
        octagon.lower_left_diagonal_x,
        octagon.upper_right_diagonal_x,
    )
}

/// The pad geometry of one padstack, as the search tree needs it.
///
/// A mirror of the slice of `fr_rules::Padstack` that produces tile shapes,
/// owned by the board for the same reason [`ClearanceTable`](crate::clearance::ClearanceTable)
/// is: `fr-board` does not depend on `fr-rules`. Shapes are stored **relative
/// to the drill centre**; the drill item translates them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Padstack {
    /// Lowest layer the padstack spans.
    pub from_layer: i32,
    /// Highest layer the padstack spans, inclusive.
    pub to_layer: i32,
    /// One entry per layer in `from_layer ..= to_layer`. `None` is upstream's
    /// null pad shape on an unused inner layer, which v2 replaces with a
    /// synthetic hole obstacle (`search-tree-obstacles.md` §9.1).
    pub shapes: Vec<Option<TileShape>>,
    /// Radius of the drilled hole, `0` for an SMD padstack. Feeds the synthetic
    /// hole obstacle.
    pub drill_radius: i32,
}

impl Padstack {
    /// A padstack with the same pad on every layer of the span.
    pub fn uniform(from_layer: i32, to_layer: i32, shape: TileShape) -> Padstack {
        let count = (to_layer - from_layer + 1).max(0) as usize;
        Padstack {
            from_layer,
            to_layer,
            shapes: vec![Some(shape); count],
            drill_radius: 0,
        }
    }

    /// A round pad of `radius` on every layer of the span, approximated by its
    /// bounding octagon — the shape a via padstack has in practice.
    pub fn round(from_layer: i32, to_layer: i32, radius: i32) -> Padstack {
        let mut padstack =
            Padstack::uniform(from_layer, to_layer, TileShape::Oct(circle_octagon(radius)));
        padstack.drill_radius = radius;
        padstack
    }

    /// The pad shape on `layer`, or `None` when the padstack does not span it
    /// or has no pad there.
    pub fn shape_on_layer(&self, layer: i32) -> Option<&TileShape> {
        if layer < self.from_layer || layer > self.to_layer {
            return None;
        }
        self.shapes
            .get((layer - self.from_layer) as usize)
            .and_then(|s| s.as_ref())
    }
}

/// The board's padstack table, indexed by
/// [`Pin::padstack_no`](crate::item::Pin) / [`Via::padstack_no`](crate::item::Via).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PadstackTable {
    padstacks: Vec<Padstack>,
}

impl PadstackTable {
    /// An empty table.
    pub fn new() -> Self {
        PadstackTable::default()
    }

    /// Appends a padstack and returns the index items refer to it by.
    pub fn push(&mut self, padstack: Padstack) -> u32 {
        self.padstacks.push(padstack);
        (self.padstacks.len() - 1) as u32
    }

    /// The padstack with this index, or `None` if the table does not hold it.
    pub fn get(&self, padstack_no: u32) -> Option<&Padstack> {
        self.padstacks.get(padstack_no as usize)
    }

    /// Number of padstacks in the table.
    pub fn len(&self) -> usize {
        self.padstacks.len()
    }

    /// True when the table is empty.
    pub fn is_empty(&self) -> bool {
        self.padstacks.is_empty()
    }
}

/// The bounding octagon of a circle of `radius` centred at the origin.
///
/// v2's synthetic drill-hole obstacle is a `Circle`, which this crate's tile
/// shapes cannot represent; the enclosing octagon is the conservative stand-in
/// (`search-tree-obstacles.md` §9.1, and see the module docs of
/// [`search_tree`](crate::search_tree) for the list of such simplifications).
pub(crate) fn circle_octagon(radius: i32) -> IntOctagon {
    let radius = radius.max(0);
    // The 45° borders of the enclosing octagon touch the circle at ±r√2, so the
    // diagonal intercepts are r√2 rounded up.
    let diagonal = ((radius as f64) * std::f64::consts::SQRT_2).ceil() as i32;
    IntOctagon::new(
        -radius, -radius, radius, radius, -diagonal, diagonal, -diagonal, diagonal,
    )
}

/// Translation as a [`Vector`], for the placement transforms.
pub(crate) fn translation(vector: IntVector) -> Vector {
    Vector::Int(vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(llx: i32, lly: i32, urx: i32, ury: i32) -> TileShape {
        TileShape::Box(IntBox::new(llx, lly, urx, ury))
    }

    #[test]
    fn bounds_of_a_box_are_the_degenerate_octagon() {
        let bounds = boxed(0, 0, 10, 20).bounds();
        assert_eq!(bounds, OctBounds::from_box(0, 0, 10, 20));
    }

    #[test]
    fn intersection_agrees_across_all_flavor_pairs() {
        let a = boxed(0, 0, 100, 100);
        let far = boxed(200, 200, 300, 300);
        let touching = boxed(100, 0, 200, 100);

        let a_oct = a.to_flavor(AngleRestriction::FortyFive);
        let a_simplex = TileShape::Simplex(IntBox::new(0, 0, 100, 100).to_simplex());
        for left in [&a, &a_oct, &a_simplex] {
            for right in [&far, &touching] {
                let expected = right == &touching;
                let right_simplex = TileShape::Simplex(match right {
                    TileShape::Box(b) => b.to_simplex(),
                    _ => unreachable!(),
                });
                assert_eq!(left.intersects(right), expected, "{left:?} vs {right:?}");
                assert_eq!(left.intersects(&right_simplex), expected);
            }
        }
    }

    #[test]
    fn the_fortyfive_flavor_keeps_a_box_that_is_only_accidentally_an_octagon() {
        // The §3.4 rule: an octagon whose 45° borders are slack is really a box,
        // and offsetting it as an octagon would chamfer its corners.
        let shape = boxed(0, 0, 100, 100);
        assert!(matches!(
            shape.to_flavor(AngleRestriction::FortyFive),
            TileShape::Box(_)
        ));

        let chamfered = TileShape::Oct(IntOctagon::new(0, 0, 100, 100, -50, 150, 20, 180));
        assert!(matches!(
            chamfered.to_flavor(AngleRestriction::FortyFive),
            TileShape::Oct(_)
        ));
    }

    #[test]
    fn enlarging_a_box_keeps_square_corners() {
        let grown = boxed(0, 0, 100, 100).enlarge(10);
        assert_eq!(grown, boxed(-10, -10, 110, 110));
    }

    #[test]
    fn sectioning_splits_only_shapes_that_are_too_wide() {
        let small = boxed(0, 0, 100, 100);
        assert_eq!(small.divide_into_sections(1000.0).len(), 1);

        let huge = boxed(0, 0, 10_000, 10_000);
        let sections = huge.divide_into_sections(1000.0);
        assert!(sections.len() > 1, "a huge pour must be gridded");
        assert!(sections
            .iter()
            .all(|s| s.bounding_box().width() as f64 <= 1000.0 + 1.0));
    }

    #[test]
    fn a_padstack_reports_only_the_layers_it_spans() {
        let padstack = Padstack::round(1, 3, 50);
        assert!(padstack.shape_on_layer(0).is_none());
        assert!(padstack.shape_on_layer(1).is_some());
        assert!(padstack.shape_on_layer(3).is_some());
        assert!(padstack.shape_on_layer(4).is_none());
    }

    #[test]
    fn the_hole_octagon_encloses_the_circle() {
        let octagon = circle_octagon(100);
        assert_eq!(octagon.left_x, -100);
        assert_eq!(octagon.right_x, 100);
        // The diagonal intercept must be at least r*sqrt(2) or the octagon would
        // cut the circle at 45°.
        assert!(octagon.upper_right_diagonal_x as f64 >= 100.0 * std::f64::consts::SQRT_2);
    }
}
