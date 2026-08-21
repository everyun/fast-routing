//! Shape trait mirroring `app.freerouting.geometry.planar.Shape`.

use crate::planar::{area::Area, float_point::FloatPoint, point::Point};

/// Interface describing functionality for simply-connected 2D shapes in the plane (no holes).
pub trait Shape: Area {
    /// Returns the length of the border of this shape.
    fn circumference(&self) -> f64;

    /// Returns the area of the shape.
    fn area(&self) -> f64;

    /// Returns the centre of gravity of this shape.
    fn centre_of_gravity(&self) -> FloatPoint;

    /// Returns true if `point` is not contained in the inside or on the boundary of the shape.
    fn is_outside(&self, point: &Point) -> bool;

    /// Returns true if `point` is contained in this shape, but not on the border.
    fn contains_inside(&self, point: &Point) -> bool;

    /// Returns true if `point` lies exactly on the boundary of the shape.
    fn contains_on_border(&self, point: &Point) -> bool;

    /// Distance between `point` and its nearest point on the shape (0 if inside).
    fn distance(&self, point: &FloatPoint) -> f64;

    /// Distance between `point` and its nearest point on the border of the shape.
    fn border_distance(&self, point: &FloatPoint) -> f64;

    /// Smallest distance from the centre of gravity to the border of the shape.
    fn smallest_radius(&self) -> f64;
}
