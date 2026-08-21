//! Trace / PolylineTrace item representing a routed wire on a copper layer.

use crate::item::ItemHeader;
use fr_geometry::planar::{IntBox, IntPoint};

/// A routed trace on a specific board layer.
#[derive(Debug, Clone, PartialEq)]
pub struct PolylineTrace {
    pub header: ItemHeader,
    pub layer: i32,
    pub half_width: i32,
    pub corner_points: Vec<IntPoint>,
}

impl PolylineTrace {
    pub fn new(
        id_no: i32,
        net_no: i32,
        clearance_class: i32,
        layer: i32,
        half_width: i32,
        corner_points: Vec<IntPoint>,
    ) -> Self {
        PolylineTrace {
            header: ItemHeader::new(id_no, vec![net_no], clearance_class, 0),
            layer,
            half_width,
            corner_points,
        }
    }

    /// Calculates the 2D bounding box of this trace.
    pub fn bounding_box(&self) -> IntBox {
        if self.corner_points.is_empty() {
            return IntBox::EMPTY;
        }
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for p in &self.corner_points {
            min_x = min_x.min(p.x - self.half_width);
            min_y = min_y.min(p.y - self.half_width);
            max_x = max_x.max(p.x + self.half_width);
            max_y = max_y.max(p.y + self.half_width);
        }
        IntBox::new(min_x, min_y, max_x, max_y)
    }

    /// Total centerline length of the trace.
    pub fn length(&self) -> f64 {
        let mut total = 0.0;
        for i in 1..self.corner_points.len() {
            total += self.corner_points[i - 1].distance(&self.corner_points[i]);
        }
        total
    }
}
