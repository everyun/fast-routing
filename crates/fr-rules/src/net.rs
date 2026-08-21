//! Describes properties for an individual electrical net.
//! Ported from `app.freerouting.rules.Net`.

use std::cmp::Ordering;

/// Describes properties for an individual electrical net.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Net {
    /// The name of the net.
    pub name: String,
    /// Subnet number (for nets divided because of from-to rules; normally 1).
    pub subnet_number: i32,
    /// Unique strict positive number of the net (1-based).
    pub net_number: i32,
    /// Indicates whether this net contains a power plane.
    pub contains_plane: bool,
    /// Index of the `NetClass` assigned to this net.
    pub net_class_index: usize,
}

impl Net {
    /// Creates a new net.
    pub fn new(
        name: impl Into<String>,
        subnet_number: i32,
        net_number: i32,
        contains_plane: bool,
        net_class_index: usize,
    ) -> Self {
        Net {
            name: name.into(),
            subnet_number,
            net_number,
            contains_plane,
            net_class_index,
        }
    }

    /// Returns the name of the net.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the subnet number.
    pub fn subnet_number(&self) -> i32 {
        self.subnet_number
    }

    /// Returns the net number.
    pub fn net_number(&self) -> i32 {
        self.net_number
    }

    /// Returns whether this net contains a power plane.
    pub fn contains_plane(&self) -> bool {
        self.contains_plane
    }

    /// Sets whether this net contains a power plane.
    pub fn set_contains_plane(&mut self, contains_plane: bool) {
        self.contains_plane = contains_plane;
    }

    /// Returns the net class index of this net.
    pub fn net_class_index(&self) -> usize {
        self.net_class_index
    }

    /// Sets the net class index of this net.
    pub fn set_net_class_index(&mut self, net_class_index: usize) {
        self.net_class_index = net_class_index;
    }
}

impl PartialOrd for Net {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Net {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.to_lowercase().cmp(&other.name.to_lowercase())
    }
}
