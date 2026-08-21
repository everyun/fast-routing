//! Circle shape in the plane defined by center and radius.
//!
//! Ported from `app.freerouting.geometry.planar.Circle`.

use crate::planar::{
    area::Area, convex_shape::ConvexShape, direction::Direction, float_point::FloatPoint,
    int_box::IntBox, int_octagon::IntOctagon, int_point::IntPoint, int_vector::IntVector,
    line::Line, point::Point, shape::Shape, simplex::Simplex, vector::Vector,
};

/// A circle in the plane defined by center point and integer radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Circle {
    pub center: IntPoint,
    pub radius: i32,
}

impl Circle {
    /// Creates a new circle. Negative radii are converted to positive.
    pub fn new(center: IntPoint, radius: i32) -> Self {
        Circle {
            center,
            radius: radius.abs(),
        }
    }

    /// Returns true if empty (circles with radius >= 0 are never empty).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Circles are always bounded.
    pub fn is_bounded(&self) -> bool {
        true
    }

    /// 0 if radius == 0, else 2.
    pub fn dimension(&self) -> i32 {
        if self.radius == 0 {
            0
        } else {
            2
        }
    }

    /// Circumference of the circle: `2 * pi * r`.
    pub fn circumference(&self) -> f64 {
        2.0 * std::f64::consts::PI * (self.radius as f64)
    }

    /// Area of the circle: `pi * r^2`.
    pub fn area(&self) -> f64 {
        std::f64::consts::PI * (self.radius as f64) * (self.radius as f64)
    }

    /// Centre of gravity of this circle.
    pub fn centre_of_gravity(&self) -> FloatPoint {
        self.center.to_float()
    }

    /// Returns true if `point` is strictly outside the circle.
    pub fn is_outside(&self, point: &Point) -> bool {
        let fp = point.to_float();
        fp.distance_square(&self.center.to_float()) > (self.radius as f64) * (self.radius as f64)
    }

    /// Returns true if `point` is inside or on the border of the circle.
    pub fn contains(&self, point: &Point) -> bool {
        !self.is_outside(point)
    }

    /// Returns true if `point` is inside or on the border.
    pub fn contains_float(&self, point: &FloatPoint) -> bool {
        point.distance_square(&self.center.to_float())
            <= (self.radius as f64) * (self.radius as f64)
    }

    /// Returns true if `point` is strictly inside the circle.
    pub fn contains_inside(&self, point: &Point) -> bool {
        let fp = point.to_float();
        fp.distance_square(&self.center.to_float()) < (self.radius as f64) * (self.radius as f64)
    }

    /// Returns true if `point` is on the border of the circle.
    pub fn contains_on_border(&self, point: &Point) -> bool {
        let fp = point.to_float();
        (fp.distance_square(&self.center.to_float()) - (self.radius as f64) * (self.radius as f64))
            .abs()
            < 1e-6
    }

    /// Distance between `point` and the circle (0 if inside).
    pub fn distance(&self, point: &FloatPoint) -> f64 {
        let d = point.distance(&self.center.to_float()) - self.radius as f64;
        d.max(0.0)
    }

    /// Distance between `point` and the circle border.
    pub fn border_distance(&self, point: &FloatPoint) -> f64 {
        let d = point.distance(&self.center.to_float()) - self.radius as f64;
        d.abs()
    }

    /// Smallest radius from center to border.
    pub fn smallest_radius(&self) -> f64 {
        self.radius as f64
    }

    /// Smallest bounding box containing this circle.
    pub fn bounding_box(&self) -> IntBox {
        IntBox::new(
            self.center.x - self.radius,
            self.center.y - self.radius,
            self.center.x + self.radius,
            self.center.y + self.radius,
        )
    }

    /// Smallest bounding octagon containing this circle.
    pub fn bounding_octagon(&self) -> IntOctagon {
        let lx = self.center.x - self.radius;
        let rx = self.center.x + self.radius;
        let ly = self.center.y - self.radius;
        let uy = self.center.y + self.radius;

        let sqrt2_minus_1 = std::f64::consts::SQRT_2 - 1.0;
        let ceil_corner = (sqrt2_minus_1 * self.radius as f64).ceil() as i32;
        let floor_corner = (sqrt2_minus_1 * self.radius as f64).floor() as i32;

        let ulx = lx - (self.center.y + floor_corner);
        let lrx = rx - (self.center.y - ceil_corner);
        let llx = lx + (self.center.y - floor_corner);
        let urx = rx + (self.center.y + ceil_corner);

        IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx)
    }

    /// Bounding tile shape around this circle with optional maximum segment length.
    pub fn bounding_tile(&self, max_segment_length: Option<i32>) -> Simplex {
        let max_seg = max_segment_length.unwrap_or(10_000);
        let quadrant_division_count = (self.radius / max_seg + 1).max(1) as usize;
        if quadrant_division_count <= 2 {
            return self.bounding_octagon().to_simplex();
        }

        let mut tangent_lines = vec![Line::new_from_coords(0, 0, 0, 0); quadrant_division_count * 4];
        for i in 0..quadrant_division_count {
            let border_delta = if i == 0 {
                IntVector::new(self.radius, 0)
            } else {
                let curr_angle = (i as f64) * std::f64::consts::PI / (2.0 * quadrant_division_count as f64);
                let curr_x = (curr_angle.sin() * self.radius as f64).ceil() as i32;
                let curr_y = (curr_angle.cos() * self.radius as f64).ceil() as i32;
                IntVector::new(curr_x, curr_y)
            };
            let curr_a = self.center.translate_by_int(&border_delta);
            let curr_b = curr_a.turn_90_degree(1, &self.center);
            let curr_dir = Direction::get_instance(&Point::Int(self.center), &Point::Int(curr_b));
            let curr_tangent = Line::new_from_point_and_dir(curr_a, &curr_dir);

            tangent_lines[quadrant_division_count + i] = curr_tangent;
            tangent_lines[2 * quadrant_division_count + i] = curr_tangent.turn_90_degree(1, &self.center);
            tangent_lines[3 * quadrant_division_count + i] = curr_tangent.turn_90_degree(2, &self.center);
            tangent_lines[i] = curr_tangent.turn_90_degree(3, &self.center);
        }

        Simplex::get_instance(&tangent_lines)
    }

    /// Checks if this circle is completely contained in `box_`.
    pub fn is_contained_in(&self, box_: &IntBox) -> bool {
        box_.ll.x <= self.center.x - self.radius
            && box_.ll.y <= self.center.y - self.radius
            && box_.ur.x >= self.center.x + self.radius
            && box_.ur.y >= self.center.y + self.radius
    }

    /// Turns this circle by `factor` * 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> Circle {
        let new_center = self.center.turn_90_degree(factor, pole);
        Circle::new(new_center, self.radius)
    }

    /// Rotates circle approximation by `angle` around `pole`.
    pub fn rotate_approx(&self, angle: f64, pole: &FloatPoint) -> Circle {
        let new_center = self.center.to_float().rotate(angle, pole).round();
        Circle::new(new_center, self.radius)
    }

    /// Mirrors circle at vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> Circle {
        let new_center = self.center.mirror_vertical(pole);
        Circle::new(new_center, self.radius)
    }

    /// Mirrors circle at horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> Circle {
        let new_center = self.center.mirror_horizontal(pole);
        Circle::new(new_center, self.radius)
    }

    /// Maximum width (diameter).
    pub fn max_width(&self) -> f64 {
        2.0 * self.radius as f64
    }

    /// Minimum width (diameter).
    pub fn min_width(&self) -> f64 {
        2.0 * self.radius as f64
    }

    /// Offsets circle radius by `offset`.
    pub fn offset(&self, offset: f64) -> Circle {
        let new_radius = (self.radius as f64 + offset).round() as i32;
        Circle::new(self.center, new_radius.max(0))
    }

    /// Shrinks circle radius by `offset` (minimum radius 1).
    pub fn shrink(&self, offset: f64) -> Circle {
        let new_radius = (self.radius as f64 - offset).round() as i32;
        Circle::new(self.center, new_radius.max(1))
    }

    /// Enlarges circle by `offset`.
    pub fn enlarge(&self, offset: f64) -> Circle {
        if offset == 0.0 {
            return *self;
        }
        let new_radius = self.radius + offset.round() as i32;
        Circle::new(self.center, new_radius.max(0))
    }

    /// Translates this circle by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Circle {
        if vector.is_zero() {
            return *self;
        }
        let iv = match vector {
            Vector::Int(iv) => *iv,
            Vector::Rational(_) => panic!("Circle translateBy only implemented for integer vectors"),
        };
        Circle::new(self.center.translate_by_int(&iv), self.radius)
    }

    /// Checks if this circle intersects another circle.
    pub fn intersects_circle(&self, other: &Circle) -> bool {
        let r_sum = (self.radius + other.radius) as f64;
        self.center.to_float().distance_square(&other.center.to_float()) <= r_sum * r_sum
    }

    /// Checks if this circle intersects an `IntBox`.
    pub fn intersects_box(&self, box_: &IntBox) -> bool {
        box_.distance(&self.center.to_float()) <= self.radius as f64
    }

    /// Checks if this circle intersects an `IntOctagon`.
    pub fn intersects_octagon(&self, oct: &IntOctagon) -> bool {
        oct.to_simplex().distance(&self.center.to_float()) <= self.radius as f64
    }

    /// Checks if this circle intersects a `Simplex`.
    pub fn intersects_simplex(&self, simplex: &Simplex) -> bool {
        simplex.distance(&self.center.to_float()) <= self.radius as f64
    }
}

impl Area for Circle {
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
        self.is_contained_in(box_)
    }

    fn bounding_box(&self) -> IntBox {
        self.bounding_box()
    }

    fn bounding_octagon(&self) -> IntOctagon {
        self.bounding_octagon()
    }

    fn contains_point(&self, point: &Point) -> bool {
        self.contains(point)
    }

    fn contains_float_point(&self, point: &FloatPoint) -> bool {
        self.contains_float(point)
    }

    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint {
        let d = from_point.distance(&self.center.to_float());
        if d <= self.radius as f64 {
            *from_point
        } else {
            let scale = self.radius as f64 / d;
            FloatPoint::new(
                self.center.x as f64 + (from_point.x - self.center.x as f64) * scale,
                self.center.y as f64 + (from_point.y - self.center.y as f64) * scale,
            )
        }
    }

    fn corner_approx_arr(&self) -> Vec<FloatPoint> {
        Vec::new()
    }
}

impl Shape for Circle {
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

impl ConvexShape for Circle {
    fn max_width(&self) -> f64 {
        self.max_width()
    }

    fn min_width(&self) -> f64 {
        self.min_width()
    }
}
