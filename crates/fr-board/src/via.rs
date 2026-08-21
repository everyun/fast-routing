//! Via item connecting multiple PCB layers.

use crate::item::ItemHeader;
use fr_geometry::planar::IntPoint;

/// A via drilled through board layers.
#[derive(Debug, Clone, PartialEq)]
pub struct Via {
    pub header: ItemHeader,
    pub center: IntPoint,
    pub padstack_name: String,
    pub first_layer: i32,
    pub last_layer: i32,
    pub pad_radius: i32,
    pub drill_radius: i32,
}

impl Via {
    pub fn new(
        id_no: i32,
        net_no: i32,
        clearance_class: i32,
        center: IntPoint,
        padstack_name: &str,
        first_layer: i32,
        last_layer: i32,
        pad_radius: i32,
        drill_radius: i32,
    ) -> Self {
        Via {
            header: ItemHeader::new(id_no, vec![net_no], clearance_class, 0),
            center,
            padstack_name: padstack_name.to_string(),
            first_layer,
            last_layer,
            pad_radius,
            drill_radius,
        }
    }
}
