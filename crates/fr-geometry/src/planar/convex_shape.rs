//! Convex shape trait mirroring `app.freerouting.geometry.planar.ConvexShape`.

use crate::planar::shape::Shape;

/// A shape where for every line segment connecting two points in the shape,
/// the entire segment is contained completely in the shape.
pub trait ConvexShape: Shape {
    /// Returns the maximum diameter of the shape.
    fn max_width(&self) -> f64;

    /// Returns the minimum diameter of the shape.
    fn min_width(&self) -> f64;
}
