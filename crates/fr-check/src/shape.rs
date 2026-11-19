//! The convex tile shape the referee measures, and the **octagon norm** it
//! measures with.
//!
//! Upstream's `TileShape` is an abstract class with three concrete subclasses
//! (`IntBox`, `IntOctagon`, `Simplex`) and a double-dispatch `intersection`.
//! `fr-geometry` ports the three shapes but not the dispatcher, so [`TileShape`]
//! is that dispatcher: one enum, one `enlarge`, one `intersection`, one
//! `dimension`.
//!
//! # Why this is not Euclidean
//!
//! Every clearance test in Freerouting is "enlarge both shapes by half the
//! clearance and ask whether they overlap"
//! (`drc-connectivity.md` §1.5, §2.1). Enlarging is a Minkowski sum with a
//! **regular octagon**, not with a disc:
//!
//! ```text
//! O_d = { |x| <= d, |y| <= d, |x - y| <= sqrt(2)*d, |x + y| <= sqrt(2)*d }
//! ```
//!
//! and `IntOctagon::offset` builds it by moving the four axis bounds by
//! `round(d)` and the four diagonal bounds by `round(sqrt(2)*d)`.
//!
//! `O_d` **contains** the disc of radius `d` and touches it only in the axis and
//! the 45° directions; halfway between (22.5°) it reaches `sec(22.5°) ≈ 1.0824`
//! times as far. So the gap Freerouting measures is *at most* the Euclidean gap,
//! and is up to 8.24 % smaller — a checker that measured Euclidean distance
//! would silently miss violations Freerouting reports. See
//! `octagon_norm_test.rs::octagon_norm_and_euclid_disagree_off_axis` for the
//! hand-computed pair that pins this down.
//!
//! The second consequence is quantisation: `round(d)` and `round(sqrt(2)*d)` are
//! integers, so results are only meaningful to ±1 board unit and a pair sitting
//! at exactly the required clearance can land on either side of the test
//! depending on which way the diagonal bound rounded.

use fr_geometry::planar::{Area, IntBox, IntOctagon, Point, Simplex};

/// A convex piece of an item's copper, as the search tree stores it.
///
/// The three variants are upstream's three `TileShape` subclasses. They are kept
/// apart rather than normalised to `Simplex` because `enlarge` and `dimension`
/// have exact closed forms on the box and the octagon, and because ~90 % of a
/// real board's tile shapes never need the line arena.
#[derive(Debug, Clone, PartialEq)]
pub enum TileShape {
    /// An axis-parallel rectangle.
    Box(IntBox),
    /// A 45°-bounded octagon.
    Octagon(IntOctagon),
    /// A general convex intersection of half-planes.
    Simplex(Simplex),
}

impl TileShape {
    /// The empty shape.
    pub const EMPTY: TileShape = TileShape::Box(IntBox::EMPTY);

    /// True when the shape encloses nothing.
    pub fn is_empty(&self) -> bool {
        match self {
            TileShape::Box(b) => b.is_empty(),
            TileShape::Octagon(o) => o.is_empty(),
            TileShape::Simplex(s) => s.is_empty(),
        }
    }

    /// `2` for an area, `1` for a segment, `0` for a point, `-1` for empty.
    ///
    /// The violation predicate is `dimension() == 2` and nothing else, so
    /// exactly-touching shapes are legal (`drc-connectivity.md` §1.5.3).
    pub fn dimension(&self) -> i32 {
        match self {
            TileShape::Box(b) => b.dimension(),
            TileShape::Octagon(o) => o.dimension(),
            TileShape::Simplex(s) => s.dimension(),
        }
    }

    /// Smallest axis-parallel box containing the shape. Used only to seed the
    /// candidate sweep; it never decides a violation.
    pub fn bounding_box(&self) -> IntBox {
        match self {
            TileShape::Box(b) => *b,
            TileShape::Octagon(o) => o.bounding_box(),
            TileShape::Simplex(s) => s.bounding_box(),
        }
    }

    /// Enclosing bounding box when `self` is bounded and non-empty; `None`
    /// for empty or unbounded shapes.
    pub fn bounded_bounding_box(&self) -> Option<IntBox> {
        match self {
            TileShape::Box(b) => (!b.is_empty()).then_some(*b),
            TileShape::Octagon(o) => (!o.is_empty()).then(|| o.bounding_box()),
            TileShape::Simplex(s) => s.bounded_bounding_box(),
        }
    }

    /// `true` iff `self ∩ other` is 2-dimensional — identical to
    /// `self.intersection(other).dimension() == 2` without materializing the
    /// mixed/simplex intersection.
    pub fn has_2dim_overlap(&self, other: &TileShape) -> bool {
        match (self, other) {
            (TileShape::Box(a), TileShape::Box(b)) => {
                !a.is_empty() && !b.is_empty() && a.overlaps(b)
            }
            (TileShape::Box(a), TileShape::Octagon(b)) => {
                a.to_int_octagon().intersection(b).normalize().dimension() == 2
            }
            (TileShape::Octagon(a), TileShape::Box(b)) => {
                a.intersection(&b.to_int_octagon()).normalize().dimension() == 2
            }
            (TileShape::Octagon(a), TileShape::Octagon(b)) => {
                a.intersection(b).normalize().dimension() == 2
            }
            (a, b) => a.to_simplex().has_2dim_overlap(&b.to_simplex()),
        }
    }

    /// True when `point` is inside the shape or on its border.
    pub fn contains_point(&self, point: &Point) -> bool {
        match self {
            TileShape::Box(b) => Area::contains_point(b, point),
            TileShape::Octagon(o) => Area::contains_point(o, point),
            TileShape::Simplex(s) => Area::contains_point(s, point),
        }
    }

    /// The octagon-norm enlargement (`drc-connectivity.md` §1.5).
    ///
    /// * `IntBox::enlarge(d) == boundingOctagon().offset(d)` — a box becomes an
    ///   octagon, which is what clips the square corners the naive half-plane
    ///   offset of a 4-line shape would leave behind.
    /// * `IntOctagon::enlarge(d) == offset(d)`.
    /// * `Simplex::enlarge(d)` translates every border line outward by `d`
    ///   (integer-rounding the new anchor) and intersects with the offset
    ///   bounding octagon.
    ///
    /// `d == 0` returns the shape unchanged *including its variant*, matching
    /// upstream's `c > 0 ? shape.enlarge(c) : shape`.
    pub fn enlarge(&self, d: f64) -> TileShape {
        if d == 0.0 {
            return self.clone();
        }
        match self {
            TileShape::Box(b) => TileShape::Octagon(b.to_int_octagon().offset(d)),
            TileShape::Octagon(o) => TileShape::Octagon(o.offset(d)),
            TileShape::Simplex(s) => TileShape::Simplex(s.enlarge(d)),
        }
    }

    /// Upstream's double-dispatched `TileShape.intersection`: box ∩ box stays a
    /// box, box/octagon ∩ box/octagon is an octagon, anything touching a simplex
    /// is a simplex.
    ///
    /// The octagon result is [`IntOctagon::normalize`]d, because the raw
    /// bound-wise intersection can leave slack bounds that make
    /// [`dimension`](TileShape::dimension) — which tests all four bound pairs
    /// for strict inequality — report an area where the true intersection is a
    /// segment or a point.
    pub fn intersection(&self, other: &TileShape) -> TileShape {
        match (self, other) {
            (TileShape::Box(a), TileShape::Box(b)) => TileShape::Box(a.intersection(b)),
            (TileShape::Box(a), TileShape::Octagon(b)) => {
                TileShape::Octagon(a.to_int_octagon().intersection(b).normalize())
            }
            (TileShape::Octagon(a), TileShape::Box(b)) => {
                TileShape::Octagon(a.intersection(&b.to_int_octagon()).normalize())
            }
            (TileShape::Octagon(a), TileShape::Octagon(b)) => {
                TileShape::Octagon(a.intersection(b).normalize())
            }
            (a, b) => TileShape::Simplex(a.to_simplex().intersection(&b.to_simplex())),
        }
    }

    /// The half-plane representation, for the mixed-variant intersection path.
    pub fn to_simplex(&self) -> Simplex {
        match self {
            TileShape::Box(b) => b.to_simplex(),
            TileShape::Octagon(o) => o.to_simplex(),
            TileShape::Simplex(s) => s.clone(),
        }
    }
}

impl From<IntBox> for TileShape {
    fn from(b: IntBox) -> Self {
        TileShape::Box(b)
    }
}

impl From<IntOctagon> for TileShape {
    fn from(o: IntOctagon) -> Self {
        TileShape::Octagon(o)
    }
}

impl From<Simplex> for TileShape {
    fn from(s: Simplex) -> Self {
        TileShape::Simplex(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enlarging_a_box_moves_axis_bounds_by_d_and_diagonals_by_sqrt2_d() {
        let b = IntBox::new(0, 0, 10, 10);
        let TileShape::Octagon(o) = TileShape::Box(b).enlarge(10.0) else {
            panic!("a box enlarges into an octagon");
        };
        // round(10) == 10, round(sqrt(2) * 10) == round(14.142) == 14
        assert_eq!(
            (o.left_x, o.bottom_y, o.right_x, o.top_y),
            (-10, -10, 20, 20)
        );
        assert_eq!(o.upper_left_diagonal_x, -10 - 14);
        assert_eq!(o.lower_right_diagonal_x, 10 + 14);
        assert_eq!(o.lower_left_diagonal_x, 0 - 14);
        assert_eq!(o.upper_right_diagonal_x, 20 + 14);
    }

    #[test]
    fn enlarge_by_zero_keeps_the_variant() {
        let s = TileShape::Box(IntBox::new(0, 0, 10, 10));
        assert!(matches!(s.enlarge(0.0), TileShape::Box(_)));
    }

    #[test]
    fn touching_shapes_are_one_dimensional_and_therefore_legal() {
        let a = TileShape::Box(IntBox::new(0, 0, 10, 10));
        let b = TileShape::Box(IntBox::new(10, 0, 20, 10));
        assert_eq!(a.intersection(&b).dimension(), 1);
        let c = TileShape::Box(IntBox::new(9, 0, 20, 10));
        assert_eq!(a.intersection(&c).dimension(), 2);
    }

    #[test]
    fn octagon_intersection_is_normalized_before_dimension_is_read() {
        // Two boxes meeting at a single corner. Enlarged by 0 they are boxes,
        // so take the octagon path explicitly: the raw bound-wise intersection
        // has left_x < right_x and bottom_y < top_y and would look 2-D, while
        // the diagonal bounds pin it to the point (10, 10).
        let a = IntBox::new(0, 0, 10, 10).to_int_octagon();
        let b = IntBox::new(10, 10, 20, 20).to_int_octagon();
        let inter = TileShape::Octagon(a).intersection(&TileShape::Octagon(b));
        assert_eq!(inter.dimension(), 0);
    }
}
