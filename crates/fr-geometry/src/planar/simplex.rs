//! Convex shape defined as the intersection of half-planes.
//!
//! Ported from `app.freerouting.geometry.planar.Simplex`.

use crate::planar::{
    area::Area,
    convex_shape::ConvexShape,
    direction::Direction,
    float_point::FloatPoint,
    int_box::IntBox,
    int_octagon::IntOctagon,
    int_point::IntPoint,
    limits::{side_of_exact, Limits},
    line::Line,
    line_segment::LineSegment,
    point::Point,
    polyline::Polyline,
    shape::Shape,
    side::Side,
    vector::Vector,
};

/// Convex shape defined as the intersection of half-planes.
/// Each half-plane is the positive/right side of a directed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simplex {
    pub arr: Vec<Line>,
}

impl Simplex {
    /// Standard empty simplex.
    pub const EMPTY: Simplex = Simplex { arr: Vec::new() };

    /// Constructs a Simplex from directed lines without normalizing.
    pub const fn new(arr: Vec<Line>) -> Self {
        Simplex { arr }
    }

    /// Creates a normalized Simplex from directed lines.
    pub fn get_instance(line_arr: &[Line]) -> Self {
        if line_arr.is_empty() {
            return Simplex::EMPTY;
        }
        let mut curr_arr = line_arr.to_vec();
        curr_arr.sort();
        let s = Simplex::new(curr_arr);
        s.remove_redundant_lines()
    }

    /// Upstream `TileShape.get_instance(Point[] p_convex_polygon)` (`TileShape.java:29-36`): the
    /// edges of a **convex** corner list, closed, as half-planes.
    ///
    /// # The two-corner form is not empty
    ///
    /// With two corners the construction yields `Line(a, b)` and `Line(b, a)` — the same geometric
    /// line traversed both ways — and [`Simplex::remove_redundant_lines`] keeps both, because
    /// neither half-plane contains the other. The result is a legal, **unbounded**, one-dimensional
    /// simplex, and upstream depends on it: a `(polygon …)` with two corners inside a padstack
    /// reaches `Library.read_padstack_scope` through `PolygonShape.split_to_convex`, comes out as
    /// this shape, fails the `dimension() < 2` test, and is rescued by `offset(1)` into an infinite
    /// **strip** one unit wide. Its `corner_approx` values are `(i32::MAX, i32::MAX)` — two
    /// parallel border lines have no intersection — which is exactly what upstream's DSN dump
    /// writes out as `Integer.MAX_VALUE / scale` (Issue179's `(polygon 1 0.01 0 -3.5 0 3.5)`).
    /// Collapsing the two-corner case to [`Simplex::EMPTY`] instead drops the pad.
    ///
    /// Fewer than two corners is [`Simplex::EMPTY`]. Upstream builds `Line(p, p)` for a single
    /// corner, whose direction is the zero vector; no half-plane means that, and every consumer of
    /// the result — `corner_is_bounded`, `dimension`, `offset` — reads the direction. A point is
    /// not representable as an intersection of half-planes, so the empty shape is the honest
    /// answer and the only one that cannot produce a zero-direction [`Line`].
    pub fn from_convex_polygon(corners: &[IntPoint]) -> Simplex {
        if corners.len() < 2 {
            return Simplex::EMPTY;
        }
        let mut lines = Vec::with_capacity(corners.len());
        for index in 0..corners.len() {
            let next = (index + 1) % corners.len();
            lines.push(Line::new(corners[index], corners[next]));
        }
        Simplex::get_instance(&lines)
    }

    /// Returns true if this simplex is empty.
    pub fn is_empty(&self) -> bool {
        self.arr.is_empty()
    }

    /// Returns the number of border lines.
    pub fn border_line_count(&self) -> usize {
        self.arr.len()
    }

    /// Returns the `no`-th border line.
    pub fn border_line(&self, no: usize) -> Line {
        self.arr[no]
    }

    /// Returns true if corner `corner_index` is bounded.
    pub fn corner_is_bounded(&self, corner_index: usize) -> bool {
        if self.arr.len() <= 1 {
            return false;
        }
        let no = corner_index % self.arr.len();
        let prev_no = if no == 0 { self.arr.len() - 1 } else { no - 1 };
        let prev_dir = self.arr[prev_no].direction().get_int_vector();
        let curr_dir = self.arr[no].direction().get_int_vector();
        prev_dir.determinant(&curr_dir) > 0
    }

    /// Returns true if the border lines `corner_index - 1` and `corner_index`
    /// meet in a single point, i.e. the corner actually exists.
    ///
    /// For parallel (or degenerate) adjacent lines `corner` returns the
    /// meaningless projective triple `(0, 0, 0)` and `corner_approx` returns
    /// `(i32::MAX, i32::MAX)`, so every corner-driven computation has to skip
    /// those indices. This is weaker than [`Simplex::corner_is_bounded`],
    /// which additionally requires the corner to be convex.
    pub fn corner_is_defined(&self, corner_index: usize) -> bool {
        if self.arr.len() < 2 {
            return false;
        }
        let no = corner_index % self.arr.len();
        let prev_no = if no == 0 { self.arr.len() - 1 } else { no - 1 };
        let prev = self.arr[prev_no];
        let curr = self.arr[no];
        let prev_delta = prev.b.difference_by_int(&prev.a);
        let curr_delta = curr.b.difference_by_int(&curr.a);
        prev_delta.determinant(&curr_delta) != 0
    }

    /// Returns true if no two adjacent border lines meet, i.e. this simplex has
    /// no corner at all. That can only happen when all border lines are
    /// parallel, so the shape is a half-plane, a strip, a line or empty.
    fn has_no_corner(&self) -> bool {
        !(0..self.arr.len()).any(|i| self.corner_is_defined(i))
    }

    /// Dimension of an intersection of half-planes whose border lines are all
    /// parallel: 2 for a half-plane or a proper strip, 1 when the two opposing
    /// bounds fall onto the same line and -1 when they cross.
    ///
    /// The generic `dimension` code cannot be used for these: it derives the
    /// answer from `arr[1].intersection(&arr[2])`, which is the degenerate
    /// triple for parallel lines, and reported a *point* for a half-plane.
    fn parallel_border_lines_dimension(&self) -> i32 {
        let first_dir = self.arr[0].direction();
        let mut innermost_forward: Option<Line> = None;
        let mut innermost_backward: Option<Line> = None;
        for line in &self.arr {
            let innermost = if line.direction() == first_dir {
                &mut innermost_forward
            } else {
                &mut innermost_backward
            };
            match innermost {
                // Nested half-planes: keep the one whose border line lies
                // strictly inside all the others seen so far.
                Some(best) if best.side_of(&line.a) == Side::OnTheRight => *best = *line,
                Some(_) => {}
                None => *innermost = Some(*line),
            }
        }
        match (innermost_forward, innermost_backward) {
            (Some(forward), Some(backward)) => match backward.side_of(&forward.a) {
                Side::OnTheLeft => -1,
                Side::Collinear => 1,
                Side::OnTheRight => 2,
            },
            // Only one of the two orientations occurs, so the intersection is
            // the innermost (unbounded) half-plane.
            _ => 2,
        }
    }

    /// Returns true if the shape of this simplex is bounded.
    pub fn is_bounded(&self) -> bool {
        if self.arr.is_empty() {
            return true;
        }
        if self.arr.len() < 3 {
            return false;
        }
        for i in 0..self.arr.len() {
            if !self.corner_is_bounded(i) {
                return false;
            }
        }
        true
    }

    /// Returns the `no`-th corner of this simplex.
    pub fn corner(&self, corner_index: usize) -> Point {
        if self.arr.is_empty() {
            return Point::Int(IntPoint::ZERO);
        }
        let no = corner_index % self.arr.len();
        let prev = if no == 0 {
            self.arr[self.arr.len() - 1]
        } else {
            self.arr[no - 1]
        };
        self.arr[no].intersection(&prev)
    }

    /// Approximation of the `no`-th corner as `FloatPoint`.
    pub fn corner_approx(&self, corner_index: usize) -> FloatPoint {
        if self.arr.is_empty() {
            return FloatPoint::ZERO;
        }
        let no = corner_index % self.arr.len();
        let prev = if no == 0 {
            self.arr[self.arr.len() - 1]
        } else {
            self.arr[no - 1]
        };
        self.arr[no].intersection_approx(&prev)
    }

    /// Returns all corner approximations.
    pub fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        (0..self.arr.len()).map(|i| self.corner_approx(i)).collect()
    }

    /// Dimension: 2 (2D), 1 (segment/line), 0 (point), -1 (empty).
    pub fn dimension(&self) -> i32 {
        if self.arr.is_empty() {
            return -1;
        }
        // A simplex whose border lines are all parallel has no corner, so
        // neither the `arr.len() > 4` shortcut nor the corner arithmetic of
        // the 3- and 4-line branches below says anything about its dimension.
        // Such simplices are not reachable from `remove_redundant_lines`, but
        // `Simplex::new` (used by `offset`, `remove_border_line` and the
        // `cutout_from` pieces) can build them.
        if self.arr.len() >= 3 && self.has_no_corner() {
            return self.parallel_border_lines_dimension();
        }
        if self.arr.len() > 4 || self.arr.len() == 1 {
            return 2;
        }
        if self.arr.len() == 2 {
            if self.arr[0].overlaps(&self.arr[1]) {
                return 1;
            }
            return 2;
        }
        if self.arr.len() == 3 {
            if self.arr[0].overlaps(&self.arr[1])
                || self.arr[0].overlaps(&self.arr[2])
                || self.arr[1].overlaps(&self.arr[2])
            {
                return 1;
            }
            let is = self.arr[1].intersection(&self.arr[2]);
            let side0 = self.arr[0].side_of_point(&is);
            if side0 == Side::OnTheRight {
                return 2;
            }
            if side0 == Side::OnTheLeft {
                return -1;
            }
            return 0;
        }
        // 4 lines
        let col02 = self.arr[0].overlaps(&self.arr[2]);
        let col13 = self.arr[1].overlaps(&self.arr[3]);
        if col02 && col13 {
            0
        } else if col02 || col13 {
            1
        } else {
            2
        }
    }

    /// Checks if this simplex can be converted to an `IntBox`.
    pub fn is_int_box(&self) -> bool {
        for i in 0..self.arr.len() {
            if !self.arr[i].is_orthogonal() || !self.corner_is_bounded(i) {
                return false;
            }
        }
        true
    }

    /// Checks if this simplex can be converted to an `IntOctagon`.
    pub fn is_int_octagon(&self) -> bool {
        for i in 0..self.arr.len() {
            if !self.arr[i].is_multiple_of_45_degree() || !self.corner_is_bounded(i) {
                return false;
            }
        }
        true
    }

    /// Converts this simplex to an `IntOctagon` if all lines are multiples of 45°.
    pub fn to_int_octagon(&self) -> Option<IntOctagon> {
        if !self.is_int_octagon() {
            return None;
        }
        if self.is_empty() {
            return Some(IntOctagon::EMPTY);
        }

        // Initialise missing bounds to the biggest octagon values (like the
        // upstream Java code) instead of i32::MIN / i32::MAX, so that
        // normalize() and the corner computations stay finite.
        let mut rx = Limits::CRIT_INT;
        let mut uy = Limits::CRIT_INT;
        let mut lrx = Limits::CRIT_INT;
        let mut urx = Limits::CRIT_INT;
        let mut lx = -Limits::CRIT_INT;
        let mut ly = -Limits::CRIT_INT;
        let mut llx = -Limits::CRIT_INT;
        let mut ulx = -Limits::CRIT_INT;

        for line in &self.arr {
            let a = line.a;
            let b = line.b;
            if a.y == b.y {
                if b.x >= a.x {
                    ly = a.y;
                }
                if b.x <= a.x {
                    uy = a.y;
                }
            }
            if a.x == b.x {
                if b.y >= a.y {
                    rx = a.x;
                }
                if b.y <= a.y {
                    lx = a.x;
                }
            }
            if a.y < b.y {
                if a.x < b.x {
                    lrx = a.x - a.y;
                } else if a.x > b.x {
                    urx = a.x + a.y;
                }
            } else if a.y > b.y {
                if a.x < b.x {
                    llx = a.x + a.y;
                } else if a.x > b.x {
                    ulx = a.x - a.y;
                }
            }
        }
        Some(IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx).normalize())
    }

    /// Translates this simplex by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Simplex {
        if vector.is_zero() {
            return self.clone();
        }
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.translate_by(vector)).collect();
        Simplex::new(new_arr)
    }

    /// Turns this simplex by `factor` * 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> Simplex {
        let new_arr: Vec<Line> = self
            .arr
            .iter()
            .map(|l| l.turn_90_degree(factor, pole))
            .collect();
        Simplex::get_instance(&new_arr)
    }

    /// Mirrors this simplex at the vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> Simplex {
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.mirror_vertical(pole)).collect();
        Simplex::get_instance(&new_arr)
    }

    /// Mirrors this simplex at the horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> Simplex {
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.mirror_horizontal(pole)).collect();
        Simplex::get_instance(&new_arr)
    }

    /// Smallest box containing all corners of this simplex.
    pub fn bounding_box(&self) -> IntBox {
        if self.arr.is_empty() {
            return IntBox::EMPTY;
        }
        let mut llx = f64::MAX;
        let mut lly = f64::MAX;
        let mut urx = f64::MIN;
        let mut ury = f64::MIN;
        for i in 0..self.arr.len() {
            let curr = self.corner_approx(i);
            llx = llx.min(curr.x);
            lly = lly.min(curr.y);
            urx = urx.max(curr.x);
            ury = ury.max(curr.y);
        }
        IntBox::new(
            llx.floor() as i32,
            lly.floor() as i32,
            urx.ceil() as i32,
            ury.ceil() as i32,
        )
    }

    /// Smallest octagon containing all corners of this simplex.
    pub fn bounding_octagon(&self) -> Option<IntOctagon> {
        if self.arr.is_empty() {
            return Some(IntOctagon::EMPTY);
        }
        let mut lx = f64::MAX;
        let mut ly = f64::MAX;
        let mut rx = f64::MIN;
        let mut uy = f64::MIN;
        let mut ulx = f64::MAX;
        let mut lrx = f64::MIN;
        let mut llx = f64::MAX;
        let mut urx = f64::MIN;
        for i in 0..self.arr.len() {
            let curr = self.corner_approx(i);
            lx = lx.min(curr.x);
            ly = ly.min(curr.y);
            rx = rx.max(curr.x);
            uy = uy.max(curr.y);

            let tmp = curr.x - curr.y;
            ulx = ulx.min(tmp);
            lrx = lrx.max(tmp);

            let tmp = curr.x + curr.y;
            llx = llx.min(tmp);
            urx = urx.max(tmp);
        }
        Some(IntOctagon::new(
            lx.floor() as i32,
            ly.floor() as i32,
            rx.ceil() as i32,
            uy.ceil() as i32,
            ulx.floor() as i32,
            lrx.ceil() as i32,
            llx.floor() as i32,
            urx.ceil() as i32,
        ))
    }

    /// Offsets the boundary of the simplex by `width`. Positive = expand outward.
    pub fn offset(&self, width: f64) -> Simplex {
        if width == 0.0 {
            return self.clone();
        }
        let new_arr: Vec<Line> = self.arr.iter().map(|l| l.translate(-width)).collect();
        let offset_simplex = Simplex::new(new_arr);
        if width < 0.0 {
            offset_simplex.remove_redundant_lines()
        } else {
            offset_simplex
        }
    }

    /// Enlarges the simplex by `offset` and clips against enlarged bounding octagon.
    pub fn enlarge(&self, offset: f64) -> Simplex {
        if offset == 0.0 {
            return self.clone();
        }
        let offset_simplex = self.offset(offset);
        if let Some(bounding_oct) = self.bounding_octagon() {
            let offset_oct = bounding_oct.offset(offset);
            offset_simplex.intersection(&offset_oct.to_simplex())
        } else {
            Simplex::EMPTY
        }
    }

    /// Shrinks the simplex by `offset`.
    pub fn shrink(&self, offset: f64) -> Simplex {
        let result = self.offset(-offset);
        if result.is_empty() {
            let centre_box = self.centre_of_gravity().round().surrounding_box();
            self.intersection(&centre_box.to_simplex())
        } else {
            result
        }
    }

    /// Intersection of two simplices.
    pub fn intersection(&self, other: &Simplex) -> Simplex {
        if self.is_empty() || other.is_empty() {
            return Simplex::EMPTY;
        }
        let mut new_arr = Vec::with_capacity(self.arr.len() + other.arr.len());
        new_arr.extend_from_slice(&self.arr);
        new_arr.extend_from_slice(&other.arr);
        new_arr.sort();
        let result = Simplex::new(new_arr);
        result.remove_redundant_lines()
    }

    /// Checks if this simplex intersects `other`.
    pub fn intersects(&self, other: &Simplex) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Whether `self ∩ other` is 2-dimensional — the exact answer of
    /// `self.intersection(other).dimension() == 2` without materializing the
    /// intersection.
    ///
    /// The fast path is the separating-axis theorem on the border lines: two
    /// bounded convex polygons with non-empty interiors fail to overlap with
    /// positive area **iff** some border line of either has the other polygon
    /// entirely in its closed opposite half-plane. Vertices are handled as
    /// homogeneous `i128` triples (the same arithmetic as
    /// [`Line::side_of_intersection`]) — no heap, no float, no `BigInt`.
    ///
    /// The order of the stages is what makes it fast on the carve loop's
    /// workload, where most pairs are rejected:
    ///
    /// 1. strict bounding-box *interior* test (the boxes are outward-rounded
    ///    hulls, so disjoint box interiors imply disjoint shape interiors);
    /// 2. the separation scan over **unverified** adjacent-pair corners —
    ///    sound because the convex hull of those corners always contains the
    ///    region (a true vertex hidden by a redundant line sits *behind* the
    ///    phantom corners that line creates), so "every corner in the closed
    ///    opposite half-plane" really does separate the regions;
    /// 3. only when nothing separates: per-corner feasibility verification
    ///    (any infeasible corner → the simplex is non-normalized or empty and
    ///    the exact construction decides) and a distinct-corner count, which
    ///    decides the degenerate (point / segment) case exactly.
    ///
    /// Unbounded or degenerate-boundary simplexes (fewer than three border
    /// lines, a non-strict turn) and `i128` overflow fall back to the exact
    /// construction. In the carve loop both operands are normalized bounded
    /// tiles, so the fallback is cold there.
    pub fn has_2dim_overlap(&self, other: &Simplex) -> bool {
        self.has_2dim_overlap_fast(other)
            .unwrap_or_else(|| self.intersection(other).dimension() == 2)
    }

    /// The SAT fast path of [`Simplex::has_2dim_overlap`] on its own; `None`
    /// means "cannot prove it cheaply — fall back to the exact construction".
    /// Public so hot callers can count how often the fallback fires.
    pub fn has_2dim_overlap_fast(&self, other: &Simplex) -> Option<bool> {
        // `bounded_bounding_box` is also the boundedness and strict-turn gate:
        // it answers `None` for unbounded or degenerate simplexes, which take
        // the fallback.
        let box_a = self.bounded_bounding_box()?;
        let box_b = other.bounded_bounding_box()?;
        if box_a.is_empty() || box_b.is_empty() {
            return Some(false);
        }
        if box_a.ur.x <= box_b.ll.x
            || box_b.ur.x <= box_a.ll.x
            || box_a.ur.y <= box_b.ll.y
            || box_b.ur.y <= box_a.ll.y
        {
            return Some(false);
        }
        self.has_2dim_overlap_sat(other)
    }

    /// The box-gate-free core of [`Simplex::has_2dim_overlap_fast`], for
    /// callers that already hold both bounding boxes and have applied the
    /// strict interior test themselves. `None` under the same conditions.
    pub fn has_2dim_overlap_sat(&self, other: &Simplex) -> Option<bool> {
        let raw_a = self.sat_corners()?;
        let raw_b = other.sat_corners()?;
        if self.sat_separates(&raw_b)? || other.sat_separates(&raw_a)? {
            return Some(false);
        }
        // Nothing separates the (unverified) corner hulls. Now verify: every
        // corner feasible ⟹ the corner hull IS the region, and the distinct
        // count decides the degenerate case exactly.
        if !self.sat_verify(&raw_a)? {
            return Some(false);
        }
        if !other.sat_verify(&raw_b)? {
            return Some(false);
        }
        Some(true)
    }

    /// The adjacent-pair corners of a bounded convex simplex, without a
    /// feasibility check. See [`Simplex::has_2dim_overlap`] for the soundness
    /// argument: the corner hull always contains the region, so a separation
    /// verdict over these corners is exact for the *reject* direction.
    ///
    /// `None` for unbounded or degenerate boundaries (a non-strict turn),
    /// fewer than three or more than 32 border lines, and `i128` overflow.
    /// Hot callers can memoize the result per shape.
    pub fn sat_corners(&self) -> Option<SatCorners> {
        sat_raw_corners(self)
    }

    /// Whether some border line of `self` puts every corner of `other` in its
    /// closed left half-plane — i.e. the regions meet in at most a line
    /// (empty, point or segment intersection, never 2-dimensional).
    pub fn sat_separates(&self, other: &SatCorners) -> Option<bool> {
        sat_separates(&self.arr, other)
    }

    /// The verification half of the SAT fast path, pair-independent: every
    /// corner feasible (on the right of, or on, every border line) and at
    /// least three of them distinct. `Some(true)` = the corner hull is a
    /// genuine polygon with non-empty interior; `Some(false)` = it is
    /// degenerate (point or segment — a 2-dim overlap is then exactly
    /// impossible); `None` = a corner is infeasible (non-normalized or empty
    /// region) or `i128` overflow — the exact construction decides.
    pub fn sat_verify(&self, corners: &SatCorners) -> Option<bool> {
        let distinct = sat_verify_corners(self, corners)?;
        Some(distinct >= 3)
    }

    /// Checks if this simplex intersects an `IntBox`.
    ///
    /// # Why there is a bounding-box gate in front of the exact test
    ///
    /// The exact test is [`Simplex::intersection`]: three heap allocations (the
    /// concatenated line array, the deduplicated one, the side cache), a sort,
    /// and an `i128` `side_of_intersection` per surviving line. Every hot caller
    /// — `ShapeSearchTree.overlapping_tree_entries` under a box query, and the
    /// obstacle gather of `complete_shape` — asks about a *small* stored shape
    /// against a *large* query box and gets "yes"; paying the full construction
    /// to learn that is what put this function at 34% of an Issue034 run.
    ///
    /// Both gates are exact, not heuristic, and both need the simplex to be
    /// **bounded**, which is why [`Simplex::bounded_bounding_box`] answers the
    /// two questions in one pass:
    ///
    /// * A bounded simplex is contained in its [`Simplex::bounding_box`] — the
    ///   `floor`/`ceil` there round outward, so the `f64` corner approximations
    ///   cannot shrink it below the true hull. `box ⊇ bounding_box ⊇ self` then
    ///   gives `self ∩ box = self`, non-empty exactly when `self` is, and
    ///   "non-empty" is `!arr.is_empty()` throughout this crate.
    /// * The same containment run backwards gives the disjoint gate: no point of
    ///   `self` lies outside `bounding_box`, so a box that misses the bounding
    ///   box misses the simplex.
    ///
    /// An **unbounded** simplex takes neither gate. Its bounding box is not a
    /// hull at all — two crossing border lines approximate to the single corner
    /// where they meet while the wedge itself runs to infinity, and parallel
    /// neighbours approximate to the `i32::MAX` sentinel of
    /// [`Line::intersection_approx`] — so both gates would be unsound, and the
    /// exact test is the only answer.
    pub fn intersects_box(&self, box_: &IntBox) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        if let Some(bounds) = self.bounded_bounding_box() {
            if !bounds.intersects(box_) {
                return false;
            }
            if box_.contains_box(&bounds) {
                return !box_.is_empty();
            }
        }
        self.intersects(&box_.to_simplex())
    }

    /// [`Simplex::bounding_box`], but `None` unless the simplex is bounded and
    /// therefore actually contained in it.
    ///
    /// This is [`Simplex::is_bounded`] fused with [`Simplex::bounding_box`] so
    /// that the gate in [`Simplex::intersects_box`] walks the border lines once.
    /// The boundedness test drops the `Direction` normalization
    /// [`Simplex::corner_is_bounded`] goes through: normalizing divides both
    /// components of a delta by a *positive* gcd, which cannot change the sign
    /// of the determinant the test reads.
    fn bounded_bounding_box(&self) -> Option<IntBox> {
        if self.arr.len() < 3 {
            // Fewer than three half-planes never bound a region — the empty
            // simplex included, which `intersects_box` has already answered.
            return None;
        }
        let mut llx = f64::MAX;
        let mut lly = f64::MAX;
        let mut urx = f64::MIN;
        let mut ury = f64::MIN;
        let mut prev = self.arr[self.arr.len() - 1];
        for curr in &self.arr {
            let prev_delta = prev.b.difference_by_int(&prev.a);
            let curr_delta = curr.b.difference_by_int(&curr.a);
            if prev_delta.determinant(&curr_delta) <= 0 {
                return None;
            }
            let corner = curr.intersection_approx(&prev);
            llx = llx.min(corner.x);
            lly = lly.min(corner.y);
            urx = urx.max(corner.x);
            ury = ury.max(corner.y);
            prev = *curr;
        }
        Some(IntBox::new(
            llx.floor() as i32,
            lly.floor() as i32,
            urx.ceil() as i32,
            ury.ceil() as i32,
        ))
    }

    /// Checks if this simplex intersects an `IntOctagon`.
    ///
    /// The same two gates as [`Simplex::intersects_box`], and sound for the same
    /// reason — an octagon is the intersection of its eight half-planes, so it
    /// contains an [`IntBox`] exactly when the box's extremes satisfy all eight,
    /// and it is contained in its own bounding box. On top of the exact test
    /// this also saves the `to_simplex` conversion, which builds eight `Line`s.
    pub fn intersects_octagon(&self, oct: &IntOctagon) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        if let Some(bounds) = self.bounded_bounding_box() {
            if !bounds.intersects(&oct.bounding_box()) {
                return false;
            }
            if octagon_contains_box(oct, &bounds) {
                return !oct.is_empty();
            }
        }
        self.intersects(&oct.to_simplex())
    }

    /// Edge number if `line` is a border line.
    pub fn border_line_index(&self, line: &Line) -> Option<usize> {
        self.arr.iter().position(|l| l == line)
    }

    /// Removes border line `no`.
    pub fn remove_border_line(&self, no: usize) -> Simplex {
        if no >= self.arr.len() {
            return self.clone();
        }
        let mut new_arr = self.arr.clone();
        new_arr.remove(no);
        Simplex::new(new_arr)
    }

    /// Removes redundant border lines. Assumes `self.arr` is sorted.
    pub fn remove_redundant_lines(&self) -> Simplex {
        if self.arr.is_empty() {
            return Simplex::EMPTY;
        }
        let mut line_arr: Vec<Line> = Vec::with_capacity(self.arr.len());
        line_arr.push(self.arr[0]);
        for line in &self.arr[1..] {
            if !line.fast_equals(line_arr.last().unwrap()) {
                line_arr.push(*line);
            }
        }

        let mut new_length = line_arr.len();
        let mut intersection_sides: Vec<Option<Side>> = vec![None; new_length];

        let mut try_again = new_length > 2;
        let mut index_of_last_removed_line = new_length;

        while try_again {
            try_again = false;
            let mut ind = 0;
            while ind < new_length {
                let prev_ind = if ind == 0 { new_length - 1 } else { ind - 1 };
                let next_ind = if ind == new_length - 1 { 0 } else { ind + 1 };

                let prev_line = line_arr[prev_ind];
                let curr_line = line_arr[ind];
                let next_line = line_arr[next_ind];

                let prev_dir = prev_line.direction().get_int_vector();
                let next_dir = next_line.direction().get_int_vector();
                let det = prev_dir.determinant(&next_dir);

                let mut remove_line = false;
                if det != 0 {
                    if intersection_sides[ind].is_none() {
                        intersection_sides[ind] =
                            Some(curr_line.side_of_intersection(&prev_line, &next_line));
                    }
                    let side = intersection_sides[ind].unwrap();
                    if det > 0 {
                        remove_line = side != Side::OnTheLeft;
                    } else if side == Side::OnTheLeft {
                        let curr_dir = curr_line.direction().get_int_vector();
                        if prev_dir.determinant(&curr_dir) > 0 {
                            new_length = 0;
                            try_again = false;
                            break;
                        }
                    }
                } else if prev_line.side_of(&next_line.a) == Side::OnTheLeft {
                    new_length = 0;
                    try_again = false;
                    break;
                }

                if remove_line {
                    try_again = true;
                    new_length -= 1;
                    line_arr.remove(ind);
                    intersection_sides.remove(ind);

                    if new_length < 3 {
                        try_again = false;
                        break;
                    }
                    let p_ind = if ind == 0 { new_length - 1 } else { ind - 1 };
                    intersection_sides[p_ind] = None;
                    let n_ind = if ind >= new_length { 0 } else { ind };
                    intersection_sides[n_ind] = None;

                    ind = ind.saturating_sub(1);
                    index_of_last_removed_line = ind;
                } else {
                    ind += 1;
                }

                if !try_again && ind >= index_of_last_removed_line {
                    break;
                }
            }
        }

        if new_length == 2 && line_arr[0].is_parallel(&line_arr[1]) {
            if line_arr[0].direction() == line_arr[1].direction() {
                if line_arr[1].side_of(&line_arr[0].a) == Side::OnTheLeft {
                    line_arr[0] = line_arr[1];
                }
                new_length = 1;
            } else if line_arr[1].side_of(&line_arr[0].a) == Side::OnTheLeft {
                new_length = 0;
            }
        }

        if new_length == 0 {
            Simplex::EMPTY
        } else {
            line_arr.truncate(new_length);
            Simplex::new(line_arr)
        }
    }

    /// Calculates division lines for `inner_corner_no` relative to `outer_simplex`.
    fn calc_division_lines(
        &self,
        inner_corner_no: usize,
        outer_simplex: &Simplex,
    ) -> Option<Vec<Line>> {
        let curr_inner_line = self.arr[inner_corner_no];
        let prev_inner_line = if inner_corner_no != 0 {
            self.arr[inner_corner_no - 1]
        } else {
            self.arr[self.arr.len() - 1]
        };
        let intersection = curr_inner_line.intersection_approx(&prev_inner_line);
        if intersection.x >= i32::MAX as f64 {
            return None;
        }
        let inner_corner = intersection.round();
        let ctolerance = 0.0001;
        let is_exact = (inner_corner.x as f64 - intersection.x).abs() < ctolerance
            && (inner_corner.y as f64 - intersection.y).abs() < ctolerance;

        if !is_exact {
            return Some(vec![prev_inner_line]);
        }

        let mut first_projection_dir = Direction::NULL;
        let mut second_projection_dir = Direction::NULL;
        let prev_inner_dir = prev_inner_line.direction().opposite().get_int_vector();
        let next_inner_dir = curr_inner_line.direction().get_int_vector();
        let mut outer_line_no = 0;
        let mut min_distance = f64::MAX;

        for _ in 0..outer_simplex.arr.len() {
            let outer_line = outer_simplex.arr[outer_line_no];
            let pt = Point::Int(inner_corner);
            let curr_proj_dir_opt = outer_line.perpendicular_direction(&pt);
            if curr_proj_dir_opt.is_none() {
                return Some(vec![Line::new(inner_corner, inner_corner)]);
            }
            let curr_projection_dir = curr_proj_dir_opt.unwrap();
            let curr_proj_vec = curr_projection_dir.get_int_vector();
            let projection_visible = prev_inner_dir.determinant(&curr_proj_vec) >= 0;

            if projection_visible {
                let mut curr_distance = outer_line.signed_distance(&inner_corner.to_float()).abs();
                let second_division_necessary = curr_proj_vec.determinant(&next_inner_dir) < 0;
                let mut curr_second_projection_dir = curr_projection_dir;

                if second_division_necessary {
                    let mut second_projection_visible = false;
                    let mut tmp_outer_line_no = outer_line_no;
                    while !second_projection_visible {
                        if tmp_outer_line_no == outer_simplex.arr.len() - 1 {
                            tmp_outer_line_no = 0;
                        } else {
                            tmp_outer_line_no += 1;
                        }
                        let next_outer = outer_simplex.arr[tmp_outer_line_no];
                        let next_proj_opt = next_outer.perpendicular_direction(&pt);
                        if next_proj_opt.is_none() {
                            return Some(vec![Line::new(inner_corner, inner_corner)]);
                        }
                        curr_second_projection_dir = next_proj_opt.unwrap();
                        let sec_proj_vec = curr_second_projection_dir.get_int_vector();
                        if curr_proj_vec.determinant(&sec_proj_vec) < 0 {
                            curr_distance = f64::MAX;
                            break;
                        }
                        second_projection_visible = sec_proj_vec.determinant(&next_inner_dir) >= 0;
                    }
                    if curr_distance < f64::MAX {
                        curr_distance += outer_simplex.arr[tmp_outer_line_no]
                            .signed_distance(&inner_corner.to_float())
                            .abs();
                    }
                }

                if curr_distance < min_distance {
                    min_distance = curr_distance;
                    first_projection_dir = curr_projection_dir;
                    second_projection_dir = curr_second_projection_dir;
                }
            }

            if outer_line_no == outer_simplex.arr.len() - 1 {
                outer_line_no = 0;
            } else {
                outer_line_no += 1;
            }
        }

        if min_distance == f64::MAX {
            return None;
        }

        if first_projection_dir == second_projection_dir {
            Some(vec![Line::new_from_point_and_dir(
                inner_corner,
                &first_projection_dir,
            )])
        } else {
            Some(vec![
                Line::new_from_point_and_dir(inner_corner, &first_projection_dir),
                Line::new_from_point_and_dir(inner_corner, &second_projection_dir),
            ])
        }
    }

    /// Cuts this simplex out of `outer_simplex`, returning convex pieces.
    pub fn cutout_from(&self, outer_simplex: &Simplex) -> Vec<Simplex> {
        if self.dimension() < 2 {
            return vec![outer_simplex.clone()];
        }
        let inner_simplex = self.intersection(outer_simplex);
        if inner_simplex.dimension() < 2 {
            return vec![outer_simplex.clone()];
        }
        let inner_corner_count = inner_simplex.arr.len();
        let mut division_line_arr = Vec::with_capacity(inner_corner_count);
        for inner_corner_no in 0..inner_corner_count {
            match inner_simplex.calc_division_lines(inner_corner_no, outer_simplex) {
                Some(div_lines) => division_line_arr.push(div_lines),
                None => return vec![outer_simplex.clone()],
            }
        }

        let mut check_cross_first_line = false;
        let mut prev_division_line: Option<Line> = None;
        let first_division_line = division_line_arr[0][0];
        let first_direction = first_division_line.direction().get_int_vector();
        let mut result_list = Vec::new();

        for inner_corner_no in 0..inner_corner_count {
            let next_corner_no = (inner_corner_no + 1) % inner_corner_count;
            let next_division_line = division_line_arr[next_corner_no][0];
            let curr_division_lines = &division_line_arr[inner_corner_no];

            if curr_division_lines.len() == 2 {
                let curr_dir = curr_division_lines[0].direction().get_int_vector();
                let mut merge_prev_division_line = false;
                let mut merge_first_division_line = false;
                if let Some(prev_div) = prev_division_line {
                    let prev_dir = prev_div.direction().get_int_vector();
                    if curr_dir.determinant(&prev_dir) > 0 {
                        merge_prev_division_line = true;
                    }
                }
                if !check_cross_first_line {
                    check_cross_first_line =
                        inner_corner_no > 0 && curr_dir.determinant(&first_direction) > 0;
                }
                if check_cross_first_line {
                    let curr_dir2 = curr_division_lines[1].direction().get_int_vector();
                    if curr_dir2.determinant(&first_direction) < 0 {
                        merge_first_division_line = true;
                    }
                }
                let mut piece_lines = Vec::new();
                piece_lines.push(Line::new(
                    curr_division_lines[1].b,
                    curr_division_lines[1].a,
                ));
                piece_lines.push(curr_division_lines[0]);
                if merge_prev_division_line {
                    piece_lines.push(prev_division_line.unwrap());
                }
                if merge_first_division_line {
                    piece_lines.push(Line::new(first_division_line.b, first_division_line.a));
                }
                let curr_piece = Simplex::new(piece_lines);
                let is = curr_piece.intersection(outer_simplex);
                if !is.is_empty() && is.dimension() == 2 {
                    result_list.push(is);
                }
            }

            let merge_next_division_line = next_division_line.b != next_division_line.a;
            let last_curr_division_line = curr_division_lines[curr_division_lines.len() - 1];
            let last_curr_dir = last_curr_division_line.direction().get_int_vector();
            let merge_last_curr_division_line =
                last_curr_division_line.b != last_curr_division_line.a;
            let mut merge_prev_division_line = false;
            let mut merge_first_division_line = false;

            if let Some(prev_div) = prev_division_line {
                let prev_dir = prev_div.direction().get_int_vector();
                if last_curr_dir.determinant(&prev_dir) > 0 {
                    merge_prev_division_line = true;
                }
            }
            if !check_cross_first_line {
                check_cross_first_line = inner_corner_no > 0
                    && last_curr_dir.determinant(&first_direction) > 0
                    && (last_curr_dir.x as i64 * first_direction.x as i64
                        + last_curr_dir.y as i64 * first_direction.y as i64)
                        < 0;
            }
            if check_cross_first_line {
                let next_dir = next_division_line.direction().get_int_vector();
                if next_dir.determinant(&first_direction) < 0 {
                    merge_first_division_line = true;
                }
            }

            let mut piece_lines = Vec::new();
            let curr_line = inner_simplex.arr[inner_corner_no];
            piece_lines.push(Line::new(curr_line.b, curr_line.a));
            if merge_next_division_line {
                piece_lines.push(Line::new(next_division_line.b, next_division_line.a));
            }
            if merge_last_curr_division_line {
                piece_lines.push(last_curr_division_line);
            }
            if merge_prev_division_line {
                piece_lines.push(prev_division_line.unwrap());
            }
            if merge_first_division_line {
                piece_lines.push(Line::new(first_division_line.b, first_division_line.a));
            }
            let curr_piece = Simplex::new(piece_lines);
            let is = curr_piece.intersection(outer_simplex);
            if !is.is_empty() && is.dimension() == 2 {
                result_list.push(is);
            }
            prev_division_line = Some(next_division_line);
        }

        result_list
    }

    /// Cuts `inner` out of this simplex.
    pub fn cutout(&self, inner: &Simplex) -> Vec<Simplex> {
        inner.cutout_from(self)
    }

    /// Calculates area using surveyor's formula.
    pub fn area(&self) -> f64 {
        if !self.is_bounded() {
            return f64::MAX;
        }
        if self.dimension() < 2 {
            return 0.0;
        }
        let corner_count = self.arr.len();
        let mut result = 0.0;
        let mut prev_corner = self.corner_approx(corner_count - 2);
        let mut curr_corner = self.corner_approx(corner_count - 1);
        for i in 0..corner_count {
            let next_corner = self.corner_approx(i);
            result += curr_corner.x * (next_corner.y - prev_corner.y);
            prev_corner = curr_corner;
            curr_corner = next_corner;
        }
        0.5 * result.abs()
    }

    /// Circumference of the bounded simplex.
    pub fn circumference(&self) -> f64 {
        if !self.is_bounded() {
            return i32::MAX as f64;
        }
        let corner_count = self.arr.len();
        let mut result = 0.0;
        let mut prev_corner = self.corner_approx(corner_count - 1);
        for i in 0..corner_count {
            let curr_corner = self.corner_approx(i);
            result += curr_corner.distance(&prev_corner);
            prev_corner = curr_corner;
        }
        result
    }

    /// Center of gravity (centroid) of the simplex corners.
    ///
    /// Corners between parallel border lines do not exist and are skipped;
    /// averaging in the `(i32::MAX, i32::MAX)` that `corner_approx` reports for
    /// them would drag the centroid off to the coordinate limit. For a bounded
    /// simplex every corner is defined, so the result is unchanged.
    pub fn centre_of_gravity(&self) -> FloatPoint {
        if self.arr.is_empty() {
            return FloatPoint::ZERO;
        }
        let mut x = 0.0;
        let mut y = 0.0;
        let mut corner_count = 0usize;
        for i in 0..self.arr.len() {
            if !self.corner_is_defined(i) {
                continue;
            }
            let p = self.corner_approx(i);
            x += p.x;
            y += p.y;
            corner_count += 1;
        }
        if corner_count == 0 {
            // All border lines are parallel, so the shape has no corner and no
            // centroid. Fall back to the mean of the points defining the border
            // lines, which is at least finite and on the boundary.
            let (mut x, mut y) = (0.0, 0.0);
            for line in &self.arr {
                x += line.a.x as f64;
                y += line.a.y as f64;
            }
            let line_count = self.arr.len() as f64;
            return FloatPoint::new(x / line_count, y / line_count);
        }
        FloatPoint::new(x / corner_count as f64, y / corner_count as f64)
    }

    /// Returns true if `point` is outside this simplex.
    pub fn is_outside(&self, point: &Point) -> bool {
        if self.arr.is_empty() {
            return true;
        }
        for line in &self.arr {
            if line.side_of_point(point) == Side::OnTheLeft {
                return true;
            }
        }
        false
    }

    /// Returns true if `point` is inside or on the border.
    pub fn contains(&self, point: &Point) -> bool {
        !self.is_outside(point)
    }

    /// Returns true if `point` is strictly inside.
    pub fn contains_inside(&self, point: &Point) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        for line in &self.arr {
            if line.side_of_point(point) != Side::OnTheRight {
                return false;
            }
        }
        true
    }

    /// Returns true if `point` is on the border.
    pub fn contains_on_border(&self, point: &Point) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        let mut on_border = false;
        for line in &self.arr {
            let side = line.side_of_point(point);
            if side == Side::OnTheLeft {
                return false;
            }
            if side == Side::Collinear {
                on_border = true;
            }
        }
        on_border
    }

    /// Returns true if `point` is contained within `tolerance`.
    pub fn contains_float(&self, point: &FloatPoint, tolerance: f64) -> bool {
        if self.arr.is_empty() {
            return false;
        }
        for line in &self.arr {
            if line.side_of_float_tol(point, tolerance) != Side::OnTheRight {
                return false;
            }
        }
        true
    }

    /// Distance between `point` and nearest point on this simplex.
    pub fn distance(&self, point: &FloatPoint) -> f64 {
        let nearest = self.nearest_point_approx(point);
        nearest.distance(point)
    }

    /// Distance between `point` and nearest border point.
    pub fn border_distance(&self, point: &FloatPoint) -> f64 {
        if let Some(nearest) = self.nearest_border_point_approx(point) {
            nearest.distance(point)
        } else {
            0.0
        }
    }

    /// Smallest radius from center of gravity to border.
    pub fn smallest_radius(&self) -> f64 {
        self.border_distance(&self.centre_of_gravity())
    }

    /// Nearest point on this simplex to `from_point`.
    pub fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        if self.contains_float(from_point, 0.0) {
            return *from_point;
        }
        self.nearest_border_point_approx(from_point)
            .unwrap_or(*from_point)
    }

    /// Nearest border point approximation.
    pub fn nearest_border_point_approx(&self, from_point: &FloatPoint) -> Option<FloatPoint> {
        let pts = self.nearest_border_points_approx(from_point, 1);
        pts.into_iter().next()
    }

    /// Approximate `count` nearest points on the border to `from_point`.
    pub fn nearest_border_points_approx(
        &self,
        from_point: &FloatPoint,
        count: usize,
    ) -> Vec<FloatPoint> {
        if count == 0 || self.arr.is_empty() {
            return Vec::new();
        }
        let line_count = self.arr.len();
        if line_count == 1 {
            return vec![from_point.projection_approx(&self.arr[0])];
        }
        if self.dimension() == 0 {
            return vec![self.corner_approx(0)];
        }
        let result_count = count.min(line_count);
        let mut nearest_points: Vec<FloatPoint> = Vec::with_capacity(result_count);
        let mut min_dists: Vec<f64> = vec![f64::MAX; result_count];

        for i in 0..line_count {
            if self.corner_is_bounded(i) {
                let curr_corner = self.corner_approx(i);
                let curr_dist = curr_corner.distance_square(from_point);
                for j in 0..result_count {
                    if curr_dist < min_dists[j] {
                        min_dists.insert(j, curr_dist);
                        min_dists.truncate(result_count);
                        nearest_points.insert(j, curr_corner);
                        nearest_points.truncate(result_count);
                        break;
                    }
                }
            }
        }

        let mut prev_ind = line_count - 2;
        let mut curr_ind = line_count - 1;

        for next_ind in 0..line_count {
            let projection = from_point.projection_approx(&self.arr[curr_ind]);
            let left_ok = !self.corner_is_bounded(curr_ind)
                || self.arr[prev_ind].side_of_float(&projection) == Side::OnTheRight;
            let right_ok = !self.corner_is_bounded(next_ind)
                || self.arr[next_ind].side_of_float(&projection) == Side::OnTheRight;

            if left_ok && right_ok {
                let curr_dist = projection.distance_square(from_point);
                for j in 0..result_count {
                    if curr_dist < min_dists[j] {
                        min_dists.insert(j, curr_dist);
                        min_dists.truncate(result_count);
                        nearest_points.insert(j, projection);
                        nearest_points.truncate(result_count);
                        break;
                    }
                }
            }
            prev_ind = curr_ind;
            curr_ind = next_ind;
        }

        nearest_points
    }

    /// Entrance points where `polyline` crosses the boundary of this simplex.
    pub fn entrance_points(&self, polyline: &Polyline) -> Vec<(usize, usize)> {
        let mut result = Vec::new();
        let mut prev_line_no: Option<usize> = None;
        let mut prev_edge_no: Option<usize> = None;
        let corners: Vec<Point> = (0..self.arr.len()).map(|i| self.corner(i)).collect();
        let bbox = self.bounding_box();

        for line_no in 1..polyline.arr.len().saturating_sub(1) {
            if let Some(seg) = LineSegment::from_polyline(polyline, line_no) {
                let curr_intersections = seg.border_intersections_lines(&self.arr, &corners, &bbox);
                for edge_no in curr_intersections {
                    if prev_line_no != Some(line_no) || prev_edge_no != Some(edge_no) {
                        result.push((line_no, edge_no));
                        prev_line_no = Some(line_no);
                        prev_edge_no = Some(edge_no);
                    }
                }
            }
        }
        result
    }

    /// Cuts out parts of `polyline` in the interior of this simplex.
    pub fn cutout_polyline(&self, polyline: &Polyline) -> Vec<Polyline> {
        let intersection_no = self.entrance_points(polyline);
        let first_corner = polyline.first_corner();
        let first_corner_inside = self.contains_inside(&first_corner);

        if intersection_no.is_empty() {
            if first_corner_inside {
                return Vec::new();
            }
            return vec![polyline.clone()];
        }

        let mut pieces = Vec::new();
        let mut curr_intersection_no = 0;
        let first_tuple = intersection_no[0];
        let first_intersection = polyline.arr[first_tuple.0].intersection(&self.arr[first_tuple.1]);

        if !first_corner_inside {
            if first_corner != first_intersection {
                let curr_polyline_no = first_tuple.0;
                let mut curr_lines = Vec::with_capacity(curr_polyline_no + 2);
                curr_lines.extend_from_slice(&polyline.arr[0..=curr_polyline_no]);
                curr_lines.push(self.arr[first_tuple.1]);
                let curr_piece = Polyline::new_from_lines(&curr_lines);
                if !curr_piece.is_empty() {
                    pieces.push(curr_piece);
                }
            }
            curr_intersection_no += 1;
        }

        while curr_intersection_no + 1 < intersection_no.len() {
            let curr_tuple = intersection_no[curr_intersection_no];
            let next_tuple = intersection_no[curr_intersection_no + 1];
            let curr_poly_no = curr_tuple.0;
            let next_poly_no = next_tuple.0;

            let mut insert_piece = false;
            for i in (curr_poly_no + 1)..next_poly_no {
                if self.is_outside(&polyline.corner(i)) {
                    insert_piece = true;
                    break;
                }
            }

            if insert_piece {
                let mut curr_lines = Vec::new();
                curr_lines.push(self.arr[curr_tuple.1]);
                curr_lines.extend_from_slice(&polyline.arr[curr_poly_no..next_poly_no]);
                curr_lines.push(self.arr[next_tuple.1]);
                let curr_piece = Polyline::new_from_lines(&curr_lines);
                if !curr_piece.is_empty() {
                    pieces.push(curr_piece);
                }
            }
            curr_intersection_no += 2;
        }

        if curr_intersection_no < intersection_no.len() {
            let curr_tuple = intersection_no[curr_intersection_no];
            let curr_poly_no = curr_tuple.0;
            let mut curr_lines = Vec::new();
            curr_lines.push(self.arr[curr_tuple.1]);
            curr_lines.extend_from_slice(&polyline.arr[curr_poly_no..]);
            let curr_piece = Polyline::new_from_lines(&curr_lines);
            if !curr_piece.is_empty() {
                pieces.push(curr_piece);
            }
        }

        pieces
    }

    /// Divides this simplex into sections of about equal size with width/height <= `max_section_width`.
    pub fn divide_into_sections(&self, max_section_width: f64) -> Vec<Simplex> {
        if self.is_empty() {
            return vec![self.clone()];
        }
        let section_boxes = self.bounding_box().divide_into_sections(max_section_width);
        let mut result = Vec::new();
        for b in section_boxes {
            let section = self.intersection(&b.to_simplex());
            if section.dimension() == 2 {
                result.push(section);
            }
        }
        result
    }
}

/// True when every point of `box_` satisfies all eight octagon constraints.
///
/// The constraints are `left_x ≤ x ≤ right_x`, `bottom_y ≤ y ≤ top_y`,
/// `upper_left_diagonal_x ≤ x − y ≤ lower_right_diagonal_x` and
/// `lower_left_diagonal_x ≤ x + y ≤ upper_right_diagonal_x`; each is linear, so
/// testing it at the two box corners that extremize it decides the whole box.
/// `i64` because `x ± y` of two `Limits::CRIT_INT` coordinates overflows `i32`.
fn octagon_contains_box(oct: &IntOctagon, box_: &IntBox) -> bool {
    let (llx, lly) = (box_.left_x() as i64, box_.bottom_y() as i64);
    let (urx, ury) = (box_.right_x() as i64, box_.top_y() as i64);
    llx >= oct.left_x as i64
        && urx <= oct.right_x as i64
        && lly >= oct.bottom_y as i64
        && ury <= oct.top_y as i64
        && (llx - ury) >= oct.upper_left_diagonal_x as i64
        && (urx - lly) <= oct.lower_right_diagonal_x as i64
        && (llx + lly) >= oct.lower_left_diagonal_x as i64
        && (urx + ury) <= oct.upper_right_diagonal_x as i64
}

impl Area for Simplex {
    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn is_bounded(&self) -> bool {
        self.is_bounded()
    }

    fn dimension(&self) -> i32 {
        self.dimension()
    }

    fn is_contained_in(&self, box_: &IntBox) -> bool {
        box_.contains_box(&self.bounding_box())
    }

    fn bounding_box(&self) -> IntBox {
        self.bounding_box()
    }

    fn bounding_octagon(&self) -> IntOctagon {
        self.bounding_octagon().unwrap_or(IntOctagon::EMPTY)
    }

    fn contains_point(&self, point: &Point) -> bool {
        self.contains(point)
    }

    fn contains_float_point(&self, point: &FloatPoint) -> bool {
        self.contains_float(point, 0.0)
    }

    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        self.nearest_point_approx(from_point)
    }

    fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        self.corner_approx_arr()
    }
}

impl Shape for Simplex {
    fn circumference(&self) -> f64 {
        self.circumference()
    }

    fn area(&self) -> f64 {
        self.area()
    }

    fn centre_of_gravity(&self) -> FloatPoint {
        self.centre_of_gravity()
    }

    fn is_outside(&self, point: &Point) -> bool {
        self.is_outside(point)
    }

    fn contains_inside(&self, point: &Point) -> bool {
        self.contains_inside(point)
    }

    fn contains_on_border(&self, point: &Point) -> bool {
        self.contains_on_border(point)
    }

    fn distance(&self, point: &FloatPoint) -> f64 {
        self.distance(point)
    }

    fn border_distance(&self, point: &FloatPoint) -> f64 {
        self.border_distance(point)
    }

    fn smallest_radius(&self) -> f64 {
        self.smallest_radius()
    }
}

impl ConvexShape for Simplex {
    fn max_width(&self) -> f64 {
        if !self.is_bounded() {
            return i32::MAX as f64;
        }
        let gravity_point = self.centre_of_gravity();
        let mut max_dist = f64::MIN;
        let mut max_dist2 = f64::MIN;
        for line in &self.arr {
            let d = line.signed_distance(&gravity_point).abs();
            if d > max_dist {
                max_dist2 = max_dist;
                max_dist = d;
            } else if d > max_dist2 {
                max_dist2 = d;
            }
        }
        max_dist + max_dist2
    }

    fn min_width(&self) -> f64 {
        if !self.is_bounded() {
            return i32::MAX as f64;
        }
        let gravity_point = self.centre_of_gravity();
        let mut min_dist = f64::MAX;
        let mut min_dist2 = f64::MAX;
        for line in &self.arr {
            let d = line.signed_distance(&gravity_point).abs();
            if d < min_dist {
                min_dist2 = min_dist;
                min_dist = d;
            } else if d < min_dist2 {
                min_dist2 = d;
            }
        }
        min_dist + min_dist2
    }
}

/// A polygon vertex as a homogeneous `i128` triple: the affine point
/// `(x / z, y / z)` with `z > 0`. This is [`Line::side_of_intersection`]'s
/// arithmetic, reused so the SAT fast path never allocates and never leaves
/// `i128`.
#[derive(Debug, Clone, Copy)]
struct SatCorner {
    x: i128,
    y: i128,
    z: i128,
}

/// The exact homogeneous intersection of two lines; `None` when they are
/// parallel or an intermediate leaves `i128` (the envelope
/// [`Line::side_of_intersection`] already declares).
fn sat_line_corner(l1: &Line, l2: &Line) -> Option<SatCorner> {
    let d1x = i128::from(l1.b.x) - i128::from(l1.a.x);
    let d1y = i128::from(l1.b.y) - i128::from(l1.a.y);
    let d2x = i128::from(l2.b.x) - i128::from(l2.a.x);
    let d2y = i128::from(l2.b.y) - i128::from(l2.a.y);
    let mut z = d2x * d1y - d2y * d1x;
    if z == 0 {
        return None;
    }
    let det1 = i128::from(l1.a.determinant(&l1.b));
    let det2 = i128::from(l2.a.determinant(&l2.b));
    let mut x = det1.checked_mul(d2x)?.checked_sub(det2.checked_mul(d1x)?)?;
    let mut y = det1.checked_mul(d2y)?.checked_sub(det2.checked_mul(d1y)?)?;
    if z < 0 {
        z = -z;
        x = -x;
        y = -y;
    }
    Some(SatCorner { x, y, z })
}

/// [`Line::side_of_point`] of a homogeneous corner, exactly; `None` on
/// overflow.
fn sat_side_of_corner(line: &Line, corner: &SatCorner) -> Option<Side> {
    let vx = i128::from(line.b.x) - i128::from(line.a.x);
    let vy = i128::from(line.b.y) - i128::from(line.a.y);
    let rel_x = corner
        .x
        .checked_sub(i128::from(line.a.x).checked_mul(corner.z)?)?;
    let rel_y = corner
        .y
        .checked_sub(i128::from(line.a.y).checked_mul(corner.z)?)?;
    let det = vx.checked_mul(rel_y)?.checked_sub(vy.checked_mul(rel_x)?)?;
    Some(side_of_exact(det).negate())
}

/// Exact projective equality of two corners; `None` on overflow.
fn sat_corner_eq(a: &SatCorner, b: &SatCorner) -> Option<bool> {
    Some(
        a.x.checked_mul(b.z)? == b.x.checked_mul(a.z)?
            && a.y.checked_mul(b.z)? == b.y.checked_mul(a.z)?,
    )
}

/// The vertex list the SAT fast path works on; see [`Simplex::sat_corners`].
/// Opaque — the only operations are the `sat_*` methods on [`Simplex`].
#[derive(Debug, Clone, Copy)]
pub struct SatCorners {
    /// The distinct feasible corners; `len` entries are valid.
    buf: [SatCorner; SAT_MAX_LINES],
    /// How many of `buf` are valid.
    len: usize,
}

/// The largest border-line count the SAT fast path handles; bigger simplexes
/// take the exact fallback. Room tiles are ≤ 8 lines in practice.
const SAT_MAX_LINES: usize = 32;

/// The adjacent-pair corners of `shape`, without any feasibility check.
///
/// `None` when the fast path cannot vouch for the answer: fewer than three or
/// more than [`SAT_MAX_LINES`] border lines, a non-strict turn between
/// adjacent lines (an unbounded or degenerate boundary — the same gate
/// [`Simplex::bounded_bounding_box`] applies), or `i128` overflow.
///
/// The corners of a *non-normalized* simplex may include phantoms (a
/// redundant line shifts the adjacent-pair intersections outward) and may
/// omit a true vertex — but the omitted vertex always lies behind the
/// phantom chain its redundant line creates, so the corners' convex hull
/// still contains the region. That containment is all the separation scan
/// needs; exactness on the no-separation side is restored by
/// [`sat_verify_corners`].
fn sat_raw_corners(shape: &Simplex) -> Option<SatCorners> {
    let lines = &shape.arr;
    let n = lines.len();
    if !(3..=SAT_MAX_LINES).contains(&n) {
        return None;
    }
    let zero = SatCorner { x: 0, y: 0, z: 1 };
    let mut out = SatCorners {
        buf: [zero; SAT_MAX_LINES],
        len: n,
    };
    for i in 0..n {
        let prev = lines[if i == 0 { n - 1 } else { i - 1 }];
        let curr = lines[i];
        // A bounded convex boundary turns strictly left at every corner. Raw
        // deltas, like `bounded_bounding_box`: dividing by a *positive* gcd
        // cannot change the determinant's sign. The gate also makes the pair
        // non-parallel, so `sat_line_corner`'s `None` here means overflow.
        let prev_delta = prev.b.difference_by_int(&prev.a);
        let curr_delta = curr.b.difference_by_int(&curr.a);
        if prev_delta.determinant(&curr_delta) <= 0 {
            return None;
        }
        out.buf[i] = sat_line_corner(&prev, &curr)?;
    }
    Some(out)
}

/// Verifies that every corner is feasible (on the right of, or on, every
/// border line) and returns the number of *distinct* corners.
///
/// All-feasible implies the corner hull is exactly the region: a strictly
/// redundant line would push its adjacent-pair corners out of the region,
/// and a line through an existing vertex leaves that vertex in the list.
/// Then `distinct` is the region's vertex count: `>= 3` is a genuine polygon,
/// `1` a point, `2` a segment. (An empty region is unreachable here — a
/// feasible corner is a region point.)
///
/// `None` on any infeasible corner or `i128` overflow: the exact
/// construction decides.
fn sat_verify_corners(shape: &Simplex, corners: &SatCorners) -> Option<usize> {
    let mut distinct = 0usize;
    for i in 0..corners.len {
        let corner = &corners.buf[i];
        for line in &shape.arr {
            if sat_side_of_corner(line, corner)? == Side::OnTheLeft {
                return None;
            }
        }
        let mut duplicate = false;
        for existing in &corners.buf[..i] {
            if sat_corner_eq(existing, corner)? {
                duplicate = true;
                break;
            }
        }
        if !duplicate {
            distinct += 1;
        }
    }
    Some(distinct)
}

/// Whether some border `line` has every corner in its closed left half-plane —
/// the polygon those corners bound then does not reach the line's right side,
/// so a shape that lives on the right side meets it in at most the line
/// itself (an empty, point or segment intersection, never 2-dimensional).
fn sat_separates(lines: &[Line], corners: &SatCorners) -> Option<bool> {
    'lines: for line in lines {
        for corner in &corners.buf[..corners.len] {
            if sat_side_of_corner(line, corner)? == Side::OnTheRight {
                continue 'lines;
            }
        }
        return Some(true);
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_simplex_octagon_round_trip_gives_box_corners() {
        let box_ = IntBox::new(0, 0, 100, 100);
        let oct = box_
            .to_simplex()
            .to_int_octagon()
            .expect("box simplex converts to an octagon");
        assert!(!oct.is_empty());
        assert_eq!(oct.bounding_box(), box_);
        // The 4 missing diagonal bounds must be normalized so that the
        // corners degenerate to the box corners without overflowing.
        assert_eq!(oct.corner(0), IntPoint::new(0, 0));
        assert_eq!(oct.corner(1), IntPoint::new(100, 0));
        assert_eq!(oct.corner(2), IntPoint::new(100, 0));
        assert_eq!(oct.corner(3), IntPoint::new(100, 100));
        assert_eq!(oct.corner(4), IntPoint::new(100, 100));
        assert_eq!(oct.corner(5), IntPoint::new(0, 100));
        assert_eq!(oct.corner(6), IntPoint::new(0, 100));
        assert_eq!(oct.corner(7), IntPoint::new(0, 0));
    }

    #[test]
    fn octagon_simplex_octagon_round_trip_is_identity() {
        let oct = IntOctagon::new(0, 0, 100, 100, -80, 80, 20, 180);
        let round_tripped = oct
            .to_simplex()
            .to_int_octagon()
            .expect("octagon simplex converts back to an octagon");
        assert_eq!(round_tripped, oct);
    }

    // ---- the degenerate two-corner form (Issue179) -----------------------------------------

    /// Issue179's padstack shape, in board units: `(polygon 1 0.01 0 -3.5 0 3.5)` on a board whose
    /// scale factor turns 3.5 into 3500.
    fn issue179_pad() -> [IntPoint; 2] {
        [IntPoint::new(0, -3500), IntPoint::new(0, 3500)]
    }

    #[test]
    fn a_two_corner_polygon_is_an_unbounded_one_dimensional_simplex() {
        let simplex = Simplex::from_convex_polygon(&issue179_pad());

        // Both half-planes survive: neither contains the other.
        assert_eq!(simplex.border_line_count(), 2);
        assert!(!simplex.is_empty());
        assert!(!simplex.is_bounded());
        assert_eq!(simplex.dimension(), 1);
        // The two border lines are the same geometric line, traversed both ways.
        assert!(simplex.arr[0].overlaps(&simplex.arr[1]));
        assert_ne!(simplex.arr[0].direction(), simplex.arr[1].direction());
    }

    #[test]
    fn the_corners_of_the_two_corner_form_are_the_int_max_upstream_dumps() {
        let simplex = Simplex::from_convex_polygon(&issue179_pad());
        // `Simplex.corner_approx` on parallel border lines: this is the value upstream's DSN
        // writer divides by the scale factor and emits.
        for index in 0..simplex.border_line_count() {
            let corner = simplex.corner_approx(index);
            assert_eq!(corner.x, i32::MAX as f64, "corner {index}");
            assert_eq!(corner.y, i32::MAX as f64, "corner {index}");
            assert!(!simplex.corner_is_defined(index));
        }
    }

    #[test]
    fn offsetting_the_two_corner_form_by_one_unit_makes_it_an_area() {
        // Upstream's `Library.read_padstack_scope` rescue: `dimension() < 2` ⇒ `offset(1)`.
        let strip = Simplex::from_convex_polygon(&issue179_pad()).offset(1.0);
        assert_eq!(strip.border_line_count(), 2);
        assert_eq!(strip.dimension(), 2);
        // A vertical strip one unit either side of x = 0, unbounded in y.
        assert!(strip.contains(&Point::Int(IntPoint::new(0, 0))));
        assert!(strip.contains(&Point::Int(IntPoint::new(0, 1_000_000))));
        assert!(!strip.contains(&Point::Int(IntPoint::new(2, 0))));
        assert!(!strip.contains(&Point::Int(IntPoint::new(-2, 0))));
        assert!(!strip.is_bounded());
    }

    #[test]
    fn a_three_corner_polygon_still_takes_the_ordinary_path() {
        let triangle = Simplex::from_convex_polygon(&[
            IntPoint::new(0, 0),
            IntPoint::new(100, 0),
            IntPoint::new(0, 100),
        ]);
        assert_eq!(triangle.border_line_count(), 3);
        assert!(triangle.is_bounded());
        assert_eq!(triangle.dimension(), 2);
    }

    #[test]
    fn fewer_than_two_corners_is_the_empty_simplex() {
        assert!(Simplex::from_convex_polygon(&[]).is_empty());
        assert!(Simplex::from_convex_polygon(&[IntPoint::new(7, 9)]).is_empty());
    }

    // ---- the `intersects_box` bounding-box gate --------------------------------------------

    /// The exact answer, i.e. what `intersects_box` was before the gate.
    fn intersects_box_exact(simplex: &Simplex, box_: &IntBox) -> bool {
        simplex.intersects(&box_.to_simplex())
    }

    #[test]
    fn bounded_bounding_box_agrees_with_is_bounded_and_bounding_box() {
        let shapes = [
            Simplex::from_convex_polygon(&[
                IntPoint::new(0, 0),
                IntPoint::new(100, 0),
                IntPoint::new(60, 90),
                IntPoint::new(10, 70),
            ]),
            Simplex::from_convex_polygon(&[
                IntPoint::new(-4_000, -7_000),
                IntPoint::new(3_333, -7_000),
                IntPoint::new(0, 5_555),
            ]),
            // Unbounded: the Issue179 two-line strip, and a wedge of two crossing lines.
            Simplex::from_convex_polygon(&issue179_pad()),
            Simplex::get_instance(&[
                Line::new(IntPoint::new(0, 0), IntPoint::new(100, 0)),
                Line::new(IntPoint::new(0, 0), IntPoint::new(0, -100)),
            ]),
            Simplex::EMPTY,
        ];
        for shape in &shapes {
            match shape.bounded_bounding_box() {
                Some(bounds) => {
                    assert!(shape.is_bounded(), "gate accepted an unbounded shape");
                    assert_eq!(bounds, shape.bounding_box());
                }
                None => assert!(
                    !shape.is_bounded() || shape.is_empty(),
                    "gate rejected a bounded shape"
                ),
            }
        }
    }

    /// The gate may not change a single answer, on either arm or in the
    /// fall-through, for boxes that contain / are contained by / straddle /
    /// touch / miss the shape.
    #[test]
    fn intersects_box_matches_the_exact_test_over_a_deterministic_sweep() {
        let shapes = [
            // Bounded, so both arms are live.
            Simplex::from_convex_polygon(&[
                IntPoint::new(0, 0),
                IntPoint::new(100, 0),
                IntPoint::new(60, 90),
                IntPoint::new(10, 70),
            ]),
            Simplex::from_convex_polygon(&[
                IntPoint::new(17, -33),
                IntPoint::new(91, 5),
                IntPoint::new(44, 61),
            ]),
            IntBox::new(-20, -20, 20, 20).to_simplex(),
            IntOctagon::new(-30, -30, 30, 30, -45, 45, -45, 45).to_simplex(),
            // Unbounded, so neither arm may fire.
            Simplex::from_convex_polygon(&issue179_pad()).offset(1.0),
            Simplex::get_instance(&[
                Line::new(IntPoint::new(0, 0), IntPoint::new(100, 0)),
                Line::new(IntPoint::new(0, 0), IntPoint::new(0, -100)),
            ]),
            Simplex::EMPTY,
        ];
        // A deterministic spread of boxes: degenerate, touching, straddling,
        // containing and far away, plus a few off-grid offsets so that the
        // `floor`/`ceil` rounding of the bounding box is exercised on both
        // sides of every border.
        let mut boxes = Vec::new();
        for &llx in &[-4_000, -101, -30, -1, 0, 1, 31, 60, 100, 101, 4_000] {
            for &lly in &[-4_000, -101, -30, -1, 0, 1, 31, 60, 100, 101, 4_000] {
                for &size in &[0, 1, 7, 59, 250, 9_000] {
                    boxes.push(IntBox::new(llx, lly, llx + size, lly + size));
                }
            }
        }
        // An inverted box is `IntBox::is_empty`; the gate must not invent a hit.
        boxes.push(IntBox::new(10, 10, 5, 5));
        boxes.push(IntBox::EMPTY);

        let mut gated_true = 0usize;
        for shape in &shapes {
            for box_ in &boxes {
                assert_eq!(
                    shape.intersects_box(box_),
                    intersects_box_exact(shape, box_),
                    "shape {shape:?} vs box {box_:?}"
                );
                gated_true += usize::from(shape.intersects_box(box_));
            }
        }
        assert!(gated_true > 0, "the sweep never produced a hit");
    }

    /// The containment arm is the one the routing profile is about, so pin that
    /// it is really taken rather than being dead weight in front of the exact
    /// test.
    #[test]
    fn a_shape_strictly_inside_the_query_box_never_reaches_the_exact_test() {
        let shape = Simplex::from_convex_polygon(&[
            IntPoint::new(0, 0),
            IntPoint::new(100, 0),
            IntPoint::new(60, 90),
        ]);
        let bounds = shape.bounded_bounding_box().expect("a triangle is bounded");
        assert_eq!(bounds, shape.bounding_box());
        let query = IntBox::new(
            bounds.left_x() - 1,
            bounds.bottom_y() - 1,
            bounds.right_x() + 1,
            bounds.top_y() + 1,
        );
        assert!(query.contains_box(&bounds));
        assert!(shape.intersects_box(&query));
        assert!(intersects_box_exact(&shape, &query));
    }

    #[test]
    fn intersects_octagon_matches_the_exact_test_over_a_deterministic_sweep() {
        let shapes = [
            Simplex::from_convex_polygon(&[
                IntPoint::new(0, 0),
                IntPoint::new(100, 0),
                IntPoint::new(60, 90),
                IntPoint::new(10, 70),
            ]),
            Simplex::from_convex_polygon(&[
                IntPoint::new(17, -33),
                IntPoint::new(91, 5),
                IntPoint::new(44, 61),
            ]),
            IntBox::new(-20, -20, 20, 20).to_simplex(),
            Simplex::from_convex_polygon(&issue179_pad()).offset(1.0),
            Simplex::EMPTY,
        ];
        let mut octagons = Vec::new();
        for &shift in &[-4_000, -60, -7, 0, 7, 60, 4_000] {
            for &size in &[1, 25, 120, 9_000] {
                octagons.push(
                    IntOctagon::new(
                        shift,
                        shift,
                        shift + size,
                        shift + size,
                        shift - size,
                        shift + size,
                        shift + shift - size,
                        shift + shift + 2 * size,
                    )
                    .normalize(),
                );
            }
        }
        octagons.push(IntOctagon::EMPTY);

        for shape in &shapes {
            for oct in &octagons {
                assert_eq!(
                    shape.intersects_octagon(oct),
                    shape.intersects(&oct.to_simplex()),
                    "shape {shape:?} vs octagon {oct:?}"
                );
            }
        }
    }

    #[test]
    fn octagon_contains_box_agrees_with_the_octagon_as_a_simplex() {
        let oct = IntOctagon::new(-100, -100, 100, 100, -150, 150, -150, 150);
        let as_simplex = oct.to_simplex();
        for &llx in &[-200, -120, -100, -40, 0, 40, 100, 120] {
            for &lly in &[-200, -120, -100, -40, 0, 40, 100, 120] {
                for &size in &[0, 1, 30, 90] {
                    let box_ = IntBox::new(llx, lly, llx + size, lly + size);
                    let by_corners = (0..4).all(|corner| {
                        let point = match corner {
                            0 => IntPoint::new(box_.left_x(), box_.bottom_y()),
                            1 => IntPoint::new(box_.right_x(), box_.bottom_y()),
                            2 => IntPoint::new(box_.right_x(), box_.top_y()),
                            _ => IntPoint::new(box_.left_x(), box_.top_y()),
                        };
                        as_simplex.contains(&Point::Int(point))
                    });
                    assert_eq!(
                        octagon_contains_box(&oct, &box_),
                        by_corners,
                        "box {box_:?}"
                    );
                }
            }
        }
    }

    /// The disjoint arm, likewise: a box outside the bounding box is answered
    /// without building the intersection.
    #[test]
    fn a_box_outside_the_bounding_box_is_rejected_by_the_gate() {
        let shape = Simplex::from_convex_polygon(&[
            IntPoint::new(0, 0),
            IntPoint::new(100, 0),
            IntPoint::new(60, 90),
        ]);
        let bounds = shape.bounded_bounding_box().expect("bounded");
        let query = IntBox::new(
            bounds.right_x() + 5,
            bounds.top_y() + 5,
            bounds.right_x() + 15,
            bounds.top_y() + 15,
        );
        assert!(!bounds.intersects(&query));
        assert!(!shape.intersects_box(&query));
        assert!(!intersects_box_exact(&shape, &query));
    }
}
