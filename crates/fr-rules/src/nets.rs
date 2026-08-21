//! Describes the collection of electrical nets on a board.
//! Ported from `app.freerouting.rules.Nets`.

use crate::net::Net;

/// The maximum legal net number for nets.
pub const MAX_LEGAL_NET_NO: i32 = 9999999;

/// The auxiliary net number for internal use.
pub const HIDDEN_NET_NO: i32 = 10000001;

/// Describes the collection of electrical nets on the board.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nets {
    net_arr: Vec<Net>,
}

impl Nets {
    /// Creates a new empty net list.
    pub fn new() -> Self {
        Nets {
            net_arr: Vec::new(),
        }
    }

    /// Returns false if `net_number` belongs to an internally used special-purpose net.
    pub fn is_normal_net_no(net_number: i32) -> bool {
        net_number > 0 && net_number <= MAX_LEGAL_NET_NO
    }

    /// Returns the biggest net number on the board (count of nets).
    pub fn max_net_no(&self) -> usize {
        self.net_arr.len()
    }

    /// Returns the number of nets.
    pub fn len(&self) -> usize {
        self.net_arr.len()
    }

    /// Returns true if there are no nets.
    pub fn is_empty(&self) -> bool {
        self.net_arr.is_empty()
    }

    /// Returns the net with the given net number (1-based), or `None` if not found.
    pub fn get(&self, net_number: i32) -> Option<&Net> {
        if net_number < 1 || (net_number as usize) > self.net_arr.len() {
            return None;
        }
        self.net_arr.get((net_number - 1) as usize)
    }

    /// Returns a mutable reference to the net with the given net number (1-based), or `None` if not found.
    pub fn get_mut(&mut self, net_number: i32) -> Option<&mut Net> {
        if net_number < 1 || (net_number as usize) > self.net_arr.len() {
            return None;
        }
        self.net_arr.get_mut((net_number - 1) as usize)
    }

    /// Returns the net with the given name and subnet number (case-insensitive), or `None` if not found.
    pub fn get_by_name_and_subnet(&self, name: &str, subnet_number: i32) -> Option<&Net> {
        self.net_arr.iter().find(|net| {
            net.name.eq_ignore_ascii_case(name) && net.subnet_number == subnet_number
        })
    }

    /// Returns all subnets with the given name (case-insensitive).
    pub fn get_by_name(&self, name: &str) -> Vec<&Net> {
        self.net_arr
            .iter()
            .filter(|net| net.name.eq_ignore_ascii_case(name))
            .collect()
    }

    /// Adds a new net with the given parameters and returns a reference to it.
    pub fn add(
        &mut self,
        name: impl Into<String>,
        subnet_number: i32,
        contains_plane: bool,
        net_class_index: usize,
    ) -> &Net {
        let new_net_no = (self.net_arr.len() + 1) as i32;
        let new_net = Net::new(name, subnet_number, new_net_no, contains_plane, net_class_index);
        self.net_arr.push(new_net);
        self.net_arr.last().unwrap()
    }

    /// Generates a new net with an automatically generated name (e.g. "net#1").
    pub fn new_net(&mut self, prefix: Option<&str>, net_class_index: usize) -> &Net {
        let next_no = self.net_arr.len() + 1;
        let pfx = prefix.unwrap_or("net#");
        let name = format!("{pfx}{next_no}");
        self.add(name, 1, false, net_class_index)
    }

    /// Returns an iterator over the nets.
    pub fn iter(&self) -> std::slice::Iter<'_, Net> {
        self.net_arr.iter()
    }

    /// Returns a mutable iterator over the nets.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Net> {
        self.net_arr.iter_mut()
    }
}

impl IntoIterator for Nets {
    type Item = Net;
    type IntoIter = std::vec::IntoIter<Net>;

    fn into_iter(self) -> Self::IntoIter {
        self.net_arr.into_iter()
    }
}

impl<'a> IntoIterator for &'a Nets {
    type Item = &'a Net;
    type IntoIter = std::slice::Iter<'a, Net>;

    fn into_iter(self) -> Self::IntoIter {
        self.net_arr.iter()
    }
}
