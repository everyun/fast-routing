//! Incomplete connections / Ratsnest tracking for DRC.

use fr_geometry::planar::IntPoint;

/// An unrouted connection air-line between two pin/pad coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct AirLine {
    pub from: IntPoint,
    pub to: IntPoint,
    pub net_no: i32,
}

/// Unconnected items and air-lines for a net.
#[derive(Debug, Clone, PartialEq)]
pub struct NetIncompletes {
    pub net_no: i32,
    pub unrouted_air_lines: Vec<AirLine>,
}

impl NetIncompletes {
    pub fn new(net_no: i32) -> Self {
        NetIncompletes {
            net_no,
            unrouted_air_lines: Vec::new(),
        }
    }

    pub fn is_completed(&self) -> bool {
        self.unrouted_air_lines.is_empty()
    }
}
