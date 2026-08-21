//! Ordered list of vias available for routing a net class.
//! Ported from `app.freerouting.rules.ViaRule`.

use crate::via_info::ViaInfo;

/// Contains an array of vias used for routing. Vias at the beginning of the array are preferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViaRule {
    /// Name of the via rule.
    pub name: String,
    list: Vec<ViaInfo>,
}

impl ViaRule {
    /// Creates an empty via rule named "empty".
    pub fn empty() -> Self {
        ViaRule {
            name: "empty".to_string(),
            list: Vec::new(),
        }
    }

    /// Creates a via rule with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        ViaRule {
            name: name.into(),
            list: Vec::new(),
        }
    }

    /// Appends a via to this rule.
    pub fn append_via(&mut self, via: ViaInfo) {
        self.list.push(via);
    }

    /// Removes a via with the given name from the rule. Returns false if not found.
    pub fn remove_via(&mut self, via_name: &str) -> bool {
        if let Some(pos) = self.list.iter().position(|v| v.name() == via_name) {
            self.list.remove(pos);
            true
        } else {
            false
        }
    }

    /// Returns the number of vias in this rule.
    pub fn via_count(&self) -> usize {
        self.list.len()
    }

    /// Returns the number of vias in this rule.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Returns true if this rule has no vias.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Returns the via at the given index.
    pub fn get_via(&self, index: usize) -> Option<&ViaInfo> {
        self.list.get(index)
    }

    /// Returns true if a via with the given name is contained in this rule.
    pub fn contains(&self, via_name: &str) -> bool {
        self.list.iter().any(|v| v.name() == via_name)
    }

    /// Returns true if this rule contains a via with the given padstack name.
    pub fn contains_padstack(&self, padstack_name: &str) -> bool {
        self.list.iter().any(|v| v.padstack().name == padstack_name)
    }

    /// Searches for a via in this rule with the given first and last layers. Returns `None` if no such via exists.
    pub fn get_layer_range(&self, from_layer: usize, to_layer: usize) -> Option<&ViaInfo> {
        self.list.iter().find(|v| {
            v.padstack().from_layer() == from_layer && v.padstack().to_layer() == to_layer
        })
    }

    /// Swaps the locations of two vias identified by name. Returns false if either was not found.
    pub fn swap(&mut self, first_name: &str, second_name: &str) -> bool {
        let pos1 = self.list.iter().position(|v| v.name() == first_name);
        let pos2 = self.list.iter().position(|v| v.name() == second_name);
        match (pos1, pos2) {
            (Some(i1), Some(i2)) => {
                if i1 != i2 {
                    self.list.swap(i1, i2);
                }
                true
            }
            _ => false,
        }
    }

    /// Swaps the locations of two vias at indices `idx1` and `idx2`.
    pub fn swap_indices(&mut self, idx1: usize, idx2: usize) -> bool {
        if idx1 < self.list.len() && idx2 < self.list.len() {
            if idx1 != idx2 {
                self.list.swap(idx1, idx2);
            }
            true
        } else {
            false
        }
    }

    /// Returns a slice of the vias in this rule.
    pub fn vias(&self) -> &[ViaInfo] {
        &self.list
    }
}
