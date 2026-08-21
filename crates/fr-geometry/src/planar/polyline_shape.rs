//! PolylineShape formed by thickening an open polyline by a given half-width.
//!
//! Ported from `app.freerouting.geometry.planar.PolylineShape`.

use crate::planar::{
    area::Area, float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon,
    int_point::IntPoint, point::Point, polyline::Polyline, shape::Shape, simplex::Simplex,
    vector::Vector,
};

/// A fattened polyline trace shape with straight/diagonal cap ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolylineShape {
    pub polyline: Polyline,
    pub half_width: i32,
}

impl PolylineShape {
    /// Creates a new PolylineShape from a polyline and integer half-width.
    pub fn new(polyline: Polyline, half_width: i32) -> Self {
        PolylineShape {
            polyline,
            half_width: half_width.abs(),
        }
    }

    /// Returns true if empty.
    pub fn is_empty(&self) -> bool {
        self.polyline.is_empty()
    }

    /// True because bounded by a box.
    pub fn is_bounded(&self) -> bool {
        true
    }

    /// 2 if not empty, -1 if empty.
    pub fn dimension(&self) -> i32 {
        if self.is_empty() {
            -1
        } else {
            2
        }
    }

    /// Smallest surrounding box.
    pub fn bounding_box(&self) -> IntBox {
        self.polyline.bounding_box().offset(self.half_width as f64)
    }

    /// Smallest surrounding octagon.
    pub fn bounding_octagon(&self) -> IntOctagon {
        self.polyline.bounding_octagon().offset(self.half_width as f64)
    }

    /// Decomposes this shape into convex Simplex pieces.
    pub fn offset_shapes(&self) -> Vec<Simplex> {
        self.polyline.offset_shapes(self.half_width)
    }

    /// Translates shape by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> PolylineShape {
        PolylineShape::new(self.polyline.translate_by(vector), self.half_width)
    }

    /// Turns shape by `factor` * 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> PolylineShape {
        PolylineShape::new(self.polyline.turn_90_degree(factor, pole), self.half_width)
    }

    /// Mirrors shape at vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> PolylineShape {
        PolylineShape::new(self.polyline.mirror_vertical(pole), self.half_width)
    }

    /// Mirrors shape at horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> PolylineShape {
        PolylineShape::new(self.polyline.mirror_horizontal(pole), self.half_width)
    }

    /// Checks if this polyline shape intersects an `IntBox`.
    pub fn intersects_box(&self, box_: &IntBox) -> bool {
        if !self.bounding_box().intersects(box_) {
            return false;
        }
        for s in self.offset_shapes() {
            if s.intersects_box(box_) {
                return true;
            }
        }
        false
    }

    /// Checks if this polyline shape intersects an `IntOctagon`.
    pub fn intersects_octagon(&self, oct: &IntOctagon) -> bool {
        if !self.bounding_octagon().intersects(oct) {
            return false;
        }
        for s in self.offset_shapes() {
            if s.intersects_octagon(oct) {
                return true;
            }
        }
        false
    }

    /// Checks if this polyline shape intersects a `Simplex`.
    pub fn intersects_simplex(&self, simplex: &Simplex) -> bool {
        if !self.bounding_box().intersects(&simplex.bounding_box()) {
            return false;
        }
        for s in self.offset_shapes() {
            if s.intersects(simplex) {
                return true;
            }
        }
        false
    }
}

impl Area for PolylineShape {
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
        self.bounding_octagon()
    }

    fn contains_point(&self, point: &Point) -> bool {
        for s in self.offset_shapes() {
            if s.contains(point) {
                return true;
            }
        }
        false
    }

    fn contains_float_point(&self, point: &FloatPoint) -> bool {
        for s in self.offset_shapes() {
            if s.contains_float(point, 0.0) {
                return true;
            }
        }
        false
    }

    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        let mut min_d = f64::MAX;
        let mut nearest = *from_point;
        for s in self.offset_shapes() {
            let p = s.nearest_point_approx(from_point);
            let d = p.distance(from_point);
            if d < min_d {
                min_d = d;
                nearest = p;
            }
        }
        nearest
    }

    fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        let mut result = Vec::new();
        for s in self.offset_shapes() {
            result.extend(s.corner_approx_arr());
        }
        result
    }
}

impl Shape for PolylineShape {
    fn circumference(&self) -> f64 {
        let mut total = 0.0;
        for s in self.offset_shapes() {
            total += s.circumference();
        }
        total
    }

    fn area(&self) -> f64 {
        let mut total = 0.0;
        for s in self.offset_shapes() {
            total += s.area();
        }
        total
    }

    fn centre_of_gravity(&self) -> FloatPoint {
        let shapes = self.offset_shapes();
        if shapes.is_empty() {
            return FloatPoint::ZERO;
        }
        let mut total_x = 0.0;
        let mut total_y = 0.0;
        let mut total_area = 0.0;
        for s in &shapes {
            let a = s.area();
            let c = s.centre_of_gravity();
            total_x += c.x * a;
            total_y += c.y * a;
            total_area += a;
        }
        if total_area > 0.0 {
            FloatPoint::new(total_x / total_area, total_y / total_area)
        } else {
            shapes[0].centre_of_gravity()
        }
    }

    fn is_outside(&self, point: &Point) -> bool {
        !self.contains_point(point)
    }

    fn contains_inside(&self, point: &Point) -> bool {
        for s in self.offset_shapes() {
            if s.contains_inside(point) {
                return true;
            }
        }
        false
    }

    fn contains_on_border(&self, point: &Point) -> bool {
        self.contains_point(point) && !self.contains_inside(point)
    }

    fn distance(&self, point: &FloatPoint) -> f64 {
        let p = self.nearest_point_approx(point);
        p.distance(point)
    }

    fn border_distance(&self, point: &FloatPoint) -> f64 {
        let mut min_d = f64::MAX;
        for s in self.offset_shapes() {
            let d = s.border_distance(point);
            if d < min_d {
                min_d = d;
            }
        }
        min_d
    }

    fn smallest_radius(&self) -> f64 {
        self.half_width as f64
    }
}
