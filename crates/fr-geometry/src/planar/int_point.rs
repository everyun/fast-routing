//! Integer point, ported from `app.freerouting.geometry.planar.IntPoint`.

use num_bigint::{BigInt, Sign};
use num_traits::Zero;

use crate::planar::{
    float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon, int_vector::IntVector,
    line::Line, point::Point, rational_point::RationalPoint, side::Side, vector::Vector,
};

/// Implementation of a point as a tuple of integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntPoint {
    pub x: i32,
    pub y: i32,
}

impl IntPoint {
    /// Creates an `IntPoint` from two integer coordinates.
    pub const fn new(x: i32, y: i32) -> Self {
        IntPoint { x, y }
    }

    /// Standard zero point.
    pub const ZERO: IntPoint = IntPoint::new(0, 0);

    /// Smallest box containing this point.
    pub fn surrounding_box(&self) -> IntBox {
        IntBox::new(self.x, self.y, self.x, self.y)
    }

    /// Smallest octagon containing this point.
    pub fn surrounding_octagon(&self) -> IntOctagon {
        let tmp1 = self.x - self.y;
        let tmp2 = self.x + self.y;
        IntOctagon::new(self.x, self.y, self.x, self.y, tmp1, tmp1, tmp2, tmp2)
    }

    /// Returns true if this point is in the interior or on the border of `box_`.
    pub fn is_contained_in(&self, box_: &IntBox) -> bool {
        self.x >= box_.ll.x && self.y >= box_.ll.y && self.x <= box_.ur.x && self.y <= box_.ur.y
    }

    /// Translates this point by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Point {
        if vector.is_zero() {
            Point::Int(*self)
        } else {
            vector.add_to_int_point(self)
        }
    }

    /// Translates this point by an `IntVector`.
    pub fn translate_by_int(&self, vector: &IntVector) -> IntPoint {
        IntPoint::new(self.x + vector.x, self.y + vector.y)
    }

    /// Difference vector `self - other`.
    pub fn difference_by_int(&self, other: &IntPoint) -> IntVector {
        IntVector::new(self.x - other.x, self.y - other.y)
    }

    /// Converts this point to a `FloatPoint`.
    pub fn to_float(&self) -> FloatPoint {
        FloatPoint::new(self.x as f64, self.y as f64)
    }

    /// Stable identifier for deterministic tie-breaking.
    pub fn get_id_no(&self) -> i32 {
        31 * self.x + self.y
    }

    /// Determinant of the vectors `(x, y)` and `(other.x, other.y)`.
    pub fn determinant(&self, other: &IntPoint) -> i64 {
        (self.x as i64) * (other.y as i64) - (self.y as i64) * (other.x as i64)
    }

    /// Side of a line on which this point lies.
    pub fn side_of(&self, line: &Line) -> Side {
        let v1 = Vector::Int(self.difference_by_int(&line.a_int()));
        let v2 = Vector::Int(line.b_int().difference_by_int(&line.a_int()));
        v1.side_of(&v2)
    }

    /// Perpendicular projection onto `line`.
    pub fn perpendicular_projection(&self, line: &Line) -> Point {
        let v = line.b_int().difference_by_int(&line.a_int());
        let vxvx = BigInt::from((v.x as i64) * (v.x as i64));
        let vyvy = BigInt::from((v.y as i64) * (v.y as i64));
        let vxvy = BigInt::from((v.x as i64) * (v.y as i64));
        let mut denominator = &vxvx + &vyvy;
        let det = BigInt::from(line.a_int().determinant(&line.b_int()));
        let point_x = BigInt::from(self.x);
        let point_y = BigInt::from(self.y);

        let tmp1 = &vxvx * &point_x;
        let tmp2 = &vxvy * &point_y;
        let tmp1 = tmp1 + tmp2;
        let tmp2 = &det * BigInt::from(v.y);
        let mut proj_x = tmp1 + tmp2;

        let tmp1 = &vxvy * &point_x;
        let tmp2 = &vyvy * &point_y;
        let tmp1 = tmp1 + tmp2;
        let tmp2 = &det * BigInt::from(v.x);
        let mut proj_y = tmp1 - tmp2;

        if !denominator.is_zero() {
            if denominator.sign() == Sign::Minus {
                denominator = -denominator;
                proj_x = -proj_x;
                proj_y = -proj_y;
            }
            if (&proj_x % &denominator).is_zero() && (&proj_y % &denominator).is_zero() {
                proj_x = &proj_x / &denominator;
                proj_y = &proj_y / &denominator;
                let xi = i32::try_from(&proj_x).expect("proj_x fits i32");
                let yi = i32::try_from(&proj_y).expect("proj_y fits i32");
                return Point::Int(IntPoint::new(xi, yi));
            }
        }
        Point::Rational(RationalPoint::new(proj_x, proj_y, denominator))
    }

    /// Signed area of the parallelogram spanned by `p2 - p1` and `self - p1`.
    pub fn signed_area(&self, p1: &IntPoint, p2: &IntPoint) -> f64 {
        let d21 = p2.difference_by_int(p1);
        let d01 = self.difference_by_int(p1);
        d21.determinant(&d01) as f64
    }

    /// Distance squared to `to_point`.
    pub fn distance_square(&self, to_point: &IntPoint) -> f64 {
        let dx = (to_point.x - self.x) as f64;
        let dy = (to_point.y - self.y) as f64;
        dx * dx + dy * dy
    }

    /// Distance to `to_point`.
    pub fn distance(&self, to_point: &IntPoint) -> f64 {
        self.distance_square(to_point).sqrt()
    }

    /// Snaps this point to an orthogonal line through `other`.
    pub fn orthogonal_projection(&self, other: &IntPoint) -> IntPoint {
        let horizontal_distance = (self.x - other.x).abs();
        let vertical_distance = (self.y - other.y).abs();
        if horizontal_distance <= vertical_distance {
            IntPoint::new(other.x, self.y)
        } else {
            IntPoint::new(self.x, other.y)
        }
    }

    /// Snaps this point to a 45° line through `other`.
    pub fn fortyfive_degree_projection(&self, other: &IntPoint) -> IntPoint {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dist0 = dx.abs() as f64;
        let dist1 = dy.abs() as f64;
        let diagonal1 = (dy as f64 - dx as f64) / 2.0;
        let diagonal2 = (dy as f64 + dx as f64) / 2.0;
        let dist2 = diagonal1.abs();
        let dist3 = diagonal2.abs();

        let mut min_dist = dist0;
        let dist_arr = [dist0, dist1, dist2, dist3];
        for d in &dist_arr[1..] {
            if *d < min_dist {
                min_dist = *d;
            }
        }

        if min_dist == dist_arr[0] {
            IntPoint::new(other.x, self.y)
        } else if min_dist == dist_arr[1] {
            IntPoint::new(self.x, other.y)
        } else if min_dist == dist_arr[2] {
            let diagonal_value = diagonal2 as i32;
            IntPoint::new(other.x + diagonal_value, other.y + diagonal_value)
        } else {
            let diagonal_value = diagonal1 as i32;
            IntPoint::new(other.x - diagonal_value, other.y + diagonal_value)
        }
    }

    /// Calculates a 45-degree corner point `p` so that lines `(self, p)` and
    /// `(p, to_point)` are multiples of 45°. Returns `None` if the line `(self,
    /// to_point)` is already a multiple of 45°.
    pub fn fortyfive_degree_corner(&self, to_point: &IntPoint, left_turn: bool) -> Option<IntPoint> {
        let dx = to_point.x - self.x;
        let dy = to_point.y - self.y;

        if dy > 0 && dy < dx {
            if left_turn {
                Some(IntPoint::new(to_point.x - dy, self.y))
            } else {
                Some(IntPoint::new(self.x + dy, to_point.y))
            }
        } else if dx > 0 && dy > dx {
            if left_turn {
                Some(IntPoint::new(to_point.x, self.y + dx))
            } else {
                Some(IntPoint::new(self.x, to_point.y - dx))
            }
        } else if dx < 0 && dy > -dx {
            if left_turn {
                Some(IntPoint::new(self.x, to_point.y + dx))
            } else {
                Some(IntPoint::new(to_point.x, self.y - dx))
            }
        } else if dy > 0 && dy < -dx {
            if left_turn {
                Some(IntPoint::new(self.x - dy, to_point.y))
            } else {
                Some(IntPoint::new(to_point.x + dy, self.y))
            }
        } else if dy < 0 && dy > dx {
            if left_turn {
                Some(IntPoint::new(to_point.x - dy, self.y))
            } else {
                Some(IntPoint::new(self.x + dy, to_point.y))
            }
        } else if dx < 0 && dy < dx {
            if left_turn {
                Some(IntPoint::new(to_point.x, self.y + dx))
            } else {
                Some(IntPoint::new(self.x, to_point.y - dx))
            }
        } else if dx > 0 && dy < -dx {
            if left_turn {
                Some(IntPoint::new(self.x, to_point.y + dx))
            } else {
                Some(IntPoint::new(to_point.x, self.y - dx))
            }
        } else if dy < 0 && dy > -dx {
            if left_turn {
                Some(IntPoint::new(self.x - dy, to_point.y))
            } else {
                Some(IntPoint::new(to_point.x + dy, self.y))
            }
        } else {
            None
        }
    }

    /// Calculates a 90-degree corner point `p`. Returns `None` if the line `(self,
    /// to_point)` is already orthogonal.
    pub fn ninety_degree_corner(&self, to_point: &IntPoint, left_turn: bool) -> Option<IntPoint> {
        let dx = to_point.x - self.x;
        let dy = to_point.y - self.y;

        if (dx > 0 && dy > 0) || (dx < 0 && dy < 0) {
            if left_turn {
                Some(IntPoint::new(to_point.x, self.y))
            } else {
                Some(IntPoint::new(self.x, to_point.y))
            }
        } else if (dx < 0 && dy > 0) || (dx > 0 && dy < 0) {
            if left_turn {
                Some(IntPoint::new(self.x, to_point.y))
            } else {
                Some(IntPoint::new(to_point.x, self.y))
            }
        } else {
            None
        }
    }

    /// Compares x coordinate with `other`.
    pub fn compare_x(&self, other: &IntPoint) -> i32 {
        self.x.cmp(&other.x) as i32
    }

    /// Compares y coordinate with `other`.
    pub fn compare_y(&self, other: &IntPoint) -> i32 {
        self.y.cmp(&other.y) as i32
    }

    /// Compares x then y.
    pub fn compare_xy(&self, other: &IntPoint) -> i32 {
        let r = self.compare_x(other);
        if r == 0 {
            self.compare_y(other)
        } else {
            r
        }
    }

    /// Turns this point by `factor` times 90° around `pole`.
    pub fn turn_90_degree(&self, factor: i32, pole: &IntPoint) -> IntPoint {
        let v = self.difference_by_int(pole);
        let turned = v.turn_90_degree_int(factor);
        pole.translate_by_int(&turned)
    }

    /// Mirrors this point at the vertical line through `pole`.
    pub fn mirror_vertical(&self, pole: &IntPoint) -> IntPoint {
        let v = self.difference_by_int(pole);
        let mirrored = v.mirror_at_y_axis_int();
        pole.translate_by_int(&mirrored)
    }

    /// Mirrors this point at the horizontal line through `pole`.
    pub fn mirror_horizontal(&self, pole: &IntPoint) -> IntPoint {
        let v = self.difference_by_int(pole);
        let mirrored = v.mirror_at_x_axis_int();
        pole.translate_by_int(&mirrored)
    }
}
