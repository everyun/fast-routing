//! Keepout obstacle and copper pour conduction area models.

use crate::item::ItemHeader;
use fr_geometry::planar::{IntBox, IntPoint};

/// A keepout obstacle area on one or more layers.
#[derive(Debug, Clone, PartialEq)]
pub struct ObstacleArea {
    pub header: ItemHeader,
    pub layer: i32,
    pub points: Vec<IntPoint>,
}

impl ObstacleArea {
    pub fn new(id_no: i32, clearance_class: i32, layer: i32, points: Vec<IntPoint>) -> Self {
        ObstacleArea {
            header: ItemHeader::new(id_no, Vec::new(), clearance_class, 0),
            layer,
            points,
        }
    }

    pub fn bounding_box(&self) -> IntBox {
        if self.points.is_empty() {
            return IntBox::EMPTY;
        }
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for p in &self.points {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        IntBox::new(min_x, min_y, max_x, max_y)
    }
}

/// A copper pour / power plane conduction area on a layer.
#[derive(Debug, Clone, PartialEq)]
pub struct ConductionArea {
    pub header: ItemHeader,
    pub layer: i32,
    pub net_no: i32,
    pub is_obstacle: bool,
    pub points: Vec<IntPoint>,
}

impl ConductionArea {
    pub fn new(
        id_no: i32,
        net_no: i32,
        clearance_class: i32,
        layer: i32,
        is_obstacle: bool,
        points: Vec<IntPoint>,
    ) -> Self {
        ConductionArea {
            header: ItemHeader::new(id_no, vec![net_no], clearance_class, 0),
            layer,
            net_no,
            is_obstacle,
            points,
        }
    }
}
