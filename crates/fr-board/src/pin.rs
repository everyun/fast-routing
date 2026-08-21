//! Pin item on a PCB component.

use crate::item::ItemHeader;
use fr_geometry::planar::{IntBox, IntPoint};

/// A component pin (pad) on the board.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub header: ItemHeader,
    pub pin_no: i32,
    pub center: IntPoint,
    pub pad_bounding_box: IntBox,
    pub first_layer: i32,
    pub last_layer: i32,
    pub drill_radius: i32,
}

impl Pin {
    pub fn new(
        id_no: i32,
        net_no: i32,
        clearance_class: i32,
        component_no: i32,
        pin_no: i32,
        center: IntPoint,
        pad_box: IntBox,
        first_layer: i32,
        last_layer: i32,
        drill_radius: i32,
    ) -> Self {
        Pin {
            header: ItemHeader::new(id_no, vec![net_no], clearance_class, component_no),
            pin_no,
            center,
            pad_bounding_box: pad_box,
            first_layer,
            last_layer,
            drill_radius,
        }
    }
}
