//! Collection of via definitions available for routing.
//! Ported from `app.freerouting.rules.ViaInfos`.

use crate::via_info::ViaInfo;

/// Contains the list of different via definitions that can be used in interactive and automatic routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViaInfos {
    list: Vec<ViaInfo>,
}

impl ViaInfos {
    /// Creates a new empty via collection.
    pub fn new() -> Self {
        ViaInfos { list: Vec::new() }
    }

    /// Adds a via definition. Returns false if insertion failed because the name already exists.
    pub fn add(&mut self, via_info: ViaInfo) -> bool {
        if self.name_exists(via_info.name()) {
            return false;
        }
        self.list.push(via_info);
        true
    }

    /// Returns the number of different vias in this collection.
    pub fn count(&self) -> usize {
        self.list.len()
    }

    /// Returns the number of different vias in this collection.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Returns true if there are no vias in the collection.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Returns the via at the given index.
    pub fn get(&self, index: usize) -> Option<&ViaInfo> {
        self.list.get(index)
    }

    /// Returns a mutable reference to the via at the given index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut ViaInfo> {
        self.list.get_mut(index)
    }

    /// Returns the via definition with the given name, or `None` if not found.
    pub fn get_by_name(&self, name: &str) -> Option<&ViaInfo> {
        self.list.iter().find(|v| v.name() == name)
    }

    /// Returns a mutable reference to the via definition with the given name, or `None` if not found.
    pub fn get_by_name_mut(&mut self, name: &str) -> Option<&mut ViaInfo> {
        self.list.iter_mut().find(|v| v.name() == name)
    }

    /// Returns true if a via definition with the given name already exists.
    pub fn name_exists(&self, name: &str) -> bool {
        self.list.iter().any(|v| v.name() == name)
    }

    /// Removes the via definition with the given name. Returns false if not found.
    pub fn remove(&mut self, name: &str) -> bool {
        if let Some(pos) = self.list.iter().position(|v| v.name() == name) {
            self.list.remove(pos);
            true
        } else {
            false
        }
    }

    /// Removes the via at the given index.
    pub fn remove_at(&mut self, index: usize) -> Option<ViaInfo> {
        if index < self.list.len() {
            Some(self.list.remove(index))
        } else {
            None
        }
    }

    /// Returns an iterator over the via definitions.
    pub fn iter(&self) -> std::slice::Iter<'_, ViaInfo> {
        self.list.iter()
    }

    /// Returns a mutable iterator over the via definitions.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, ViaInfo> {
        self.list.iter_mut()
    }
}

impl IntoIterator for ViaInfos {
    type Item = ViaInfo;
    type IntoIter = std::vec::IntoIter<ViaInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.list.into_iter()
    }
}

impl<'a> IntoIterator for &'a ViaInfos {
    type Item = &'a ViaInfo;
    type IntoIter = std::slice::Iter<'a, ViaInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}
