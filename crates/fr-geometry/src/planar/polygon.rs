//! Simple polygon defined by a sequence of corner points.
//!
//! Ported from `app.freerouting.geometry.planar.Polygon`.

use crate::planar::{point::Point, side::Side};

/// A Polygon is a list of points in the plane where no 2 consecutive points
/// may be equal and no 3 consecutive points collinear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polygon {
    corners: Vec<Point>,
}

impl Polygon {
    /// Creates a polygon from a slice of points. Multiple consecutive points
    /// and collinear points with their neighbours are removed.
    pub fn new(point_arr: &[Point]) -> Self {
        if point_arr.is_empty() {
            return Polygon {
                corners: Vec::new(),
            };
        }
        let mut corners: Vec<Point> = point_arr.to_vec();

        let mut corner_removed = true;
        while corner_removed {
            corner_removed = false;
            if corners.is_empty() {
                break;
            }

            // Remove consecutive equal points
            let mut i = 0;
            while i + 1 < corners.len() {
                if corners[i] == corners[i + 1] {
                    corners.remove(i + 1);
                    corner_removed = true;
                } else {
                    i += 1;
                }
            }

            // Remove points collinear with previous and next
            if corners.len() >= 3 {
                let mut j = 1;
                while j + 1 < corners.len() {
                    let prev = &corners[j - 1];
                    let curr = &corners[j];
                    let next = &corners[j + 1];
                    if curr.side_of_points(prev, next) == Side::Collinear {
                        corners.remove(j);
                        corner_removed = true;
                        break;
                    }
                    j += 1;
                }
            }
        }

        Polygon { corners }
    }

    /// Returns the slice / array of corners of this polygon.
    pub fn corner_array(&self) -> &[Point] {
        &self.corners
    }

    /// Reverts the order of the corners of this polygon.
    pub fn revert_corners(&self) -> Polygon {
        let mut rev = self.corners.clone();
        rev.reverse();
        Polygon { corners: rev }
    }

    /// Returns the winding number of this polygon, treated as closed.
    /// Positive (> 0) for counter-clockwise, negative (< 0) for clockwise.
    pub fn winding_number_after_closing(&self) -> i32 {
        let corner_arr = &self.corners;
        if corner_arr.len() < 2 {
            return 0;
        }
        let first_side_vector = corner_arr[1].difference_by(&corner_arr[0]);
        let mut prev_side_vector = first_side_vector.clone();
        let mut corner_count = corner_arr.len();
        if corner_arr[0] == corner_arr[corner_count - 1] {
            corner_count -= 1;
        }
        let mut angle_sum = 0.0;
        for i in 1..corner_count.saturating_sub(1) {
            let next_side_vector = corner_arr[i + 1].difference_by(&corner_arr[i]);
            let diff = next_side_vector.angle_approx() - prev_side_vector.angle_approx();
            angle_sum += diff;
            prev_side_vector = next_side_vector;
        }
        if corner_count > 1 {
            let next_side_vector = corner_arr[0].difference_by(&corner_arr[corner_count - 1]);
            let diff = next_side_vector.angle_approx() - prev_side_vector.angle_approx();
            angle_sum += diff;
            prev_side_vector = next_side_vector;
        }
        let diff = first_side_vector.angle_approx() - prev_side_vector.angle_approx();
        angle_sum += diff;
        angle_sum /= 2.0 * std::f64::consts::PI;
        angle_sum.round() as i32
    }
}
