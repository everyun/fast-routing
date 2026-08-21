//! Area trait mirroring `app.freerouting.geometry.planar.Area`.

use crate::planar::{
    float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon, point::Point,
};

/// An Area is a 2-dimensional geometric region in the plane, which may contain holes.
pub trait Area {
    /// Returns true if the area is empty.
    fn is_empty(&self) -> bool;

    /// Returns true if the area is contained in a sufficiently large box.
    fn is_bounded(&self) -> bool;

    /// Returns 2 if the area contains 2D shapes, 1 for curves, 0 for points, -1 for empty.
    fn dimension(&self) -> i32;

    /// Checks if this area is completely contained in `box_`.
    fn is_contained_in(&self, box_: &IntBox) -> bool;

    /// Smallest surrounding box of the area.
    fn bounding_box(&self) -> IntBox;

    /// Smallest surrounding octagon of the area.
    fn bounding_octagon(&self) -> IntOctagon;

    /// Returns true if `point` is inside or on the border of this area (not in a hole).
    fn contains_point(&self, point: &Point) -> bool;

    /// Returns true if `point` is inside this area (not in a hole).
    fn contains_float_point(&self, point: &FloatPoint) -> bool;

    /// Calculates approximation of nearest point of this area to `from_point`.
    fn nearest_point_approx(&self, from_point: &FloatPoint) -> FloatPoint;

    /// Approximations of the corners of this area.
    fn corner_approx_arr(&self) -> Vec<FloatPoint>;
}
