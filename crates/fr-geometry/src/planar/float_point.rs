//! Double-precision point approximation, ported from
//! `app.freerouting.geometry.planar.FloatPoint`.

use crate::planar::{
    direction::Direction, float_line::FloatLine, int_box::IntBox, int_octagon::IntOctagon,
    int_point::IntPoint, line::Line, side::Side,
};

/// A point in the plane with `f64` coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatPoint {
    pub x: f64,
    pub y: f64,
}

impl FloatPoint {
    /// Creates an instance from two floats.
    pub const fn new(x: f64, y: f64) -> Self {
        FloatPoint { x, y }
    }

    /// The zero point.
    pub const ZERO: FloatPoint = FloatPoint::new(0.0, 0.0);

    /// Creates a `FloatPoint` from an `IntPoint`.
    pub fn from_int_point(pt: &IntPoint) -> Self {
        FloatPoint::new(pt.x as f64, pt.y as f64)
    }

    /// Smallest `IntOctagon` containing all the input points.
    pub fn bounding_octagon(point_arr: &[FloatPoint]) -> IntOctagon {
        let mut lx = f64::MAX;
        let mut ly = f64::MAX;
        let mut rx = f64::MIN;
        let mut uy = f64::MIN;
        let mut ulx = f64::MAX;
        let mut lrx = f64::MIN;
        let mut llx = f64::MAX;
        let mut urx = f64::MIN;
        for curr in point_arr {
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
        IntOctagon::new(
            lx.floor() as i32,
            ly.floor() as i32,
            rx.ceil() as i32,
            uy.ceil() as i32,
            ulx.floor() as i32,
            lrx.ceil() as i32,
            llx.floor() as i32,
            urx.ceil() as i32,
        )
    }

    /// Square of the distance from this point to the zero point.
    pub fn size_square(&self) -> f64 {
        self.x * self.x + self.y * self.y
    }

    /// Distance from this point to the zero point.
    pub fn size(&self) -> f64 {
        self.size_square().sqrt()
    }

    /// Square of the distance to `other`.
    pub fn distance_square(&self, other: &FloatPoint) -> f64 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        dx * dx + dy * dy
    }

    /// Distance to `other`.
    pub fn distance(&self, other: &FloatPoint) -> f64 {
        self.distance_square(other).sqrt()
    }

    /// Computes the weighted distance to `other`.
    pub fn weighted_distance(
        &self,
        other: &FloatPoint,
        horizontal_weight: f64,
        vertical_weight: f64,
    ) -> f64 {
        let mut delta_x = self.x - other.x;
        let mut delta_y = self.y - other.y;
        delta_x *= horizontal_weight;
        delta_y *= vertical_weight;
        (delta_x * delta_x + delta_y * delta_y).sqrt()
    }

    /// Rounds coordinates to an `IntPoint`.
    pub fn round(&self) -> IntPoint {
        IntPoint::new(self.x.round() as i32, self.y.round() as i32)
    }

    /// Rounds this point so that if it is on the right side of a line with
    /// direction `dir`, the result point will also be on the right.
    pub fn round_to_the_right(&self, dir: &Direction) -> IntPoint {
        let direction_vector = dir.get_vector().to_float();
        let rounded_x = if direction_vector.y > 0.0 {
            self.x.ceil() as i32
        } else if direction_vector.y < 0.0 {
            self.x.floor() as i32
        } else {
            self.x.round() as i32
        };

        let rounded_y = if direction_vector.x > 0.0 {
            self.y.floor() as i32
        } else if direction_vector.x < 0.0 {
            self.y.ceil() as i32
        } else {
            self.y.round() as i32
        };

        IntPoint::new(rounded_x, rounded_y)
    }

    /// Rounds coordinates to the given integer grid.
    pub fn round_to_grid(&self, horizontal_grid: i32, vertical_grid: i32) -> IntPoint {
        let rounded_x = if horizontal_grid > 0 {
            (self.x / horizontal_grid as f64).round() * horizontal_grid as f64
        } else {
            self.x
        };
        let rounded_y = if vertical_grid > 0 {
            (self.y / vertical_grid as f64).round() * vertical_grid as f64
        } else {
            self.y
        };
        IntPoint::new(rounded_x as i32, rounded_y as i32)
    }

    /// Rounds this point to the left side of `dir`.
    pub fn round_to_the_left(&self, dir: &Direction) -> IntPoint {
        let direction_vector = dir.get_vector().to_float();
        let rounded_x = if direction_vector.y > 0.0 {
            self.x.floor() as i32
        } else if direction_vector.y < 0.0 {
            self.x.ceil() as i32
        } else {
            self.x.round() as i32
        };

        let rounded_y = if direction_vector.x > 0.0 {
            self.y.ceil() as i32
        } else if direction_vector.x < 0.0 {
            self.y.floor() as i32
        } else {
            self.y.round() as i32
        };

        IntPoint::new(rounded_x, rounded_y)
    }

    /// Vector addition.
    pub fn add(&self, other: &FloatPoint) -> FloatPoint {
        FloatPoint::new(self.x + other.x, self.y + other.y)
    }

    /// Vector subtraction.
    pub fn subtract(&self, other: &FloatPoint) -> FloatPoint {
        FloatPoint::new(self.x - other.x, self.y - other.y)
    }

    /// Approximation of perpendicular projection onto `line`.
    pub fn projection_approx(&self, line: &Line) -> FloatPoint {
        let float_line = FloatLine::new(line.a_int().to_float(), line.b_int().to_float());
        float_line.perpendicular_projection(self)
    }

    /// Scalar product `(p1 - self) . (p2 - self)`.
    pub fn scalar_product(&self, p1: &FloatPoint, p2: &FloatPoint) -> f64 {
        let dx1 = p1.x - self.x;
        let dx2 = p2.x - self.x;
        let dy1 = p1.y - self.y;
        let dy2 = p2.y - self.y;
        dx1 * dx2 + dy1 * dy2
    }

    /// Changes the size (length from zero) of this point.
    pub fn change_size(&self, new_size: f64) -> FloatPoint {
        if self.x == 0.0 && self.y == 0.0 {
            return *self;
        }
        let length = self.size();
        let new_x = (self.x * new_size) / length;
        let new_y = (self.y * new_size) / length;
        FloatPoint::new(new_x, new_y)
    }

    /// Point on line `(self, to_point)` with distance `new_length` from `self`.
    pub fn change_length(&self, to_point: &FloatPoint, new_length: f64) -> FloatPoint {
        let dx = to_point.x - self.x;
        let dy = to_point.y - self.y;
        if dx == 0.0 && dy == 0.0 {
            return *to_point;
        }
        let length = (dx * dx + dy * dy).sqrt();
        let new_x = self.x + (dx * new_length) / length;
        let new_y = self.y + (dy * new_length) / length;
        FloatPoint::new(new_x, new_y)
    }

    /// Middle point with `to_point`.
    pub fn middle_point(&self, to_point: &FloatPoint) -> FloatPoint {
        FloatPoint::new(0.5 * (self.x + to_point.x), 0.5 * (self.y + to_point.y))
    }

    /// Side of line through `p1` and `p2`.
    pub fn side_of(&self, p1: &FloatPoint, p2: &FloatPoint) -> Side {
        let d21_x = p2.x - p1.x;
        let d21_y = p2.y - p1.y;
        let d01_x = self.x - p1.x;
        let d01_y = self.y - p1.y;
        let determinant = d21_x * d01_y - d21_y * d01_x;
        Side::of(determinant)
    }

    /// Rotates by `angle` (radians) around `pole`.
    pub fn rotate(&self, angle: f64, pole: &FloatPoint) -> FloatPoint {
        if angle == 0.0 {
            return *self;
        }
        let dx = self.x - pole.x;
        let dy = self.y - pole.y;
        let sin_angle = angle.sin();
        let cos_angle = angle.cos();
        let new_dx = dx * cos_angle - dy * sin_angle;
        let new_dy = dx * sin_angle + dy * cos_angle;
        FloatPoint::new(pole.x + new_dx, pole.y + new_dy)
    }

    /// Turns by `factor` times 90° around ZERO.
    pub fn turn_90_degree(&self, factor: i32) -> FloatPoint {
        let mut n = factor % 4;
        if n < 0 {
            n += 4;
        }
        let (new_x, new_y) = match n {
            0 => (self.x, self.y),
            1 => (-self.y, self.x),
            2 => (-self.x, -self.y),
            3 => (self.y, -self.x),
            _ => unreachable!(),
        };
        FloatPoint::new(new_x, new_y)
    }

    /// Turns by `factor` times 90° around `pole`.
    pub fn turn_90_degree_around(&self, factor: i32, pole: &FloatPoint) -> FloatPoint {
        let v = self.subtract(pole);
        let turned = v.turn_90_degree(factor);
        pole.add(&turned)
    }

    /// Checks if this point is in the box spanned by `p1` and `p2` within tolerance.
    pub fn is_contained_in_box(&self, p1: &FloatPoint, p2: &FloatPoint, tolerance: f64) -> bool {
        let (min_x, max_x) = if p1.x < p2.x {
            (p1.x, p2.x)
        } else {
            (p2.x, p1.x)
        };
        if self.x < min_x - tolerance || self.x > max_x + tolerance {
            return false;
        }
        let (min_y, max_y) = if p1.y < p2.y {
            (p1.y, p2.y)
        } else {
            (p2.y, p1.y)
        };
        self.y >= min_y - tolerance && self.y <= max_y + tolerance
    }

    /// Smallest `IntBox` containing this point.
    pub fn bounding_box(&self) -> IntBox {
        let lower_left = IntPoint::new(self.x.floor() as i32, self.y.floor() as i32);
        let upper_right = IntPoint::new(self.x.ceil() as i32, self.y.ceil() as i32);
        IntBox::new_from_points(lower_left, upper_right)
    }

    /// Tangent points from this point to a circle at `to_point` with radius `distance`.
    pub fn tangential_points(&self, to_point: &FloatPoint, distance: f64) -> Vec<FloatPoint> {
        let dx_abs = (self.x - to_point.x).abs();
        let dy_abs = (self.y - to_point.y).abs();
        let situation_turned = dy_abs > dx_abs;
        let (pole, circle_center) = if situation_turned {
            (
                FloatPoint::new(-self.y, self.x),
                FloatPoint::new(-to_point.y, to_point.x),
            )
        } else {
            (*self, *to_point)
        };

        let dx = pole.x - circle_center.x;
        let dy = pole.y - circle_center.y;
        let dx_square = dx * dx;
        let dy_square = dy * dy;
        let dist_square = dx_square + dy_square;
        let radius_square = distance * distance;
        let discriminant = radius_square * dy_square - (radius_square - dx_square) * dist_square;

        if discriminant <= 0.0 {
            return Vec::new();
        }
        let square_root = discriminant.sqrt();

        let a1 = radius_square * dy;
        let dy1 = (a1 + distance * square_root) / dist_square;
        let dy2 = (a1 - distance * square_root) / dist_square;

        let first_point_y = dy1 + circle_center.y;
        let first_point_x = (radius_square - dy * dy1) / dx + circle_center.x;
        let second_point_y = dy2 + circle_center.y;
        let second_point_x = (radius_square - dy * dy2) / dx + circle_center.x;

        if situation_turned {
            vec![
                FloatPoint::new(first_point_y, -first_point_x),
                FloatPoint::new(second_point_y, -second_point_x),
            ]
        } else {
            vec![
                FloatPoint::new(first_point_x, first_point_y),
                FloatPoint::new(second_point_x, second_point_y),
            ]
        }
    }

    /// Left tangent point.
    pub fn left_tangential_point(
        &self,
        to_point: &FloatPoint,
        distance: f64,
    ) -> Option<FloatPoint> {
        let pts = self.tangential_points(to_point, distance);
        if pts.len() < 2 {
            None
        } else if to_point.side_of(self, &pts[0]) == Side::OnTheRight {
            Some(pts[0])
        } else {
            Some(pts[1])
        }
    }

    /// Right tangent point.
    pub fn right_tangential_point(
        &self,
        to_point: &FloatPoint,
        distance: f64,
    ) -> Option<FloatPoint> {
        let pts = self.tangential_points(to_point, distance);
        if pts.len() < 2 {
            None
        } else if to_point.side_of(self, &pts[0]) == Side::OnTheLeft {
            Some(pts[0])
        } else {
            Some(pts[1])
        }
    }

    /// Circumcenter of the triangle `(self, p1, p2)`.
    pub fn circle_center(&self, p1: &FloatPoint, p2: &FloatPoint) -> FloatPoint {
        let slope1 = (p1.y - self.y) / (p1.x - self.x);
        let slope2 = (p2.y - p1.y) / (p2.x - p1.x);
        let center_x = (slope1 * slope2 * (self.y - p2.y) + slope2 * (self.x + p1.x)
            - slope1 * (p1.x + p2.x))
            / (2.0 * (slope2 - slope1));
        let center_y = (0.5 * (self.x + p1.x) - center_x) / slope1 + 0.5 * (self.y + p1.y);
        FloatPoint::new(center_x, center_y)
    }

    /// Returns true if this point is in the circle through `p1, p2, p3`.
    pub fn inside_circle(&self, p1: &FloatPoint, p2: &FloatPoint, p3: &FloatPoint) -> bool {
        let center = p1.circle_center(p2, p3);
        let radius_square = center.distance_square(p1);
        self.distance_square(&center) < radius_square - 1.0
    }
}
