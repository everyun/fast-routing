//! Clearance violation model for DRC.

use fr_geometry::planar::IntPoint;

/// Record of a clearance violation between two board items.
#[derive(Debug, Clone, PartialEq)]
pub struct ClearanceViolation {
    pub first_item_id: i32,
    pub second_item_id: i32,
    pub layer: i32,
    pub required_clearance: f64,
    pub actual_distance: f64,
    pub location: IntPoint,
}

impl ClearanceViolation {
    pub fn new(
        first_item_id: i32,
        second_item_id: i32,
        layer: i32,
        required_clearance: f64,
        actual_distance: f64,
        location: IntPoint,
    ) -> Self {
        ClearanceViolation {
            first_item_id,
            second_item_id,
            layer,
            required_clearance,
            actual_distance,
            location,
        }
    }
}
