//! Collection of net classes for interactive and automatic routing.
//! Ported from `app.freerouting.rules.NetClasses`.

use crate::layer::LayerStructure;
use crate::net_class::NetClass;

/// Contains the array of net classes for interactive routing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetClasses {
    class_arr: Vec<NetClass>,
}

impl NetClasses {
    /// Creates a new empty collection of net classes.
    pub fn new() -> Self {
        NetClasses {
            class_arr: Vec::new(),
        }
    }

    /// Returns the number of classes in this collection.
    pub fn count(&self) -> usize {
        self.class_arr.len()
    }

    /// Returns the number of classes in this collection.
    pub fn len(&self) -> usize {
        self.class_arr.len()
    }

    /// Returns true if there are no net classes in the collection.
    pub fn is_empty(&self) -> bool {
        self.class_arr.is_empty()
    }

    /// Returns the net class with the given index.
    pub fn get(&self, index: usize) -> Option<&NetClass> {
        self.class_arr.get(index)
    }

    /// Returns a mutable reference to the net class with the given index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut NetClass> {
        self.class_arr.get_mut(index)
    }

    /// Returns the net class with the given name, or `None` if no such class exists.
    pub fn get_by_name(&self, name: &str) -> Option<&NetClass> {
        self.class_arr.iter().find(|c| c.name == name)
    }

    /// Returns a mutable reference to the net class with the given name, or `None` if no such class exists.
    pub fn get_by_name_mut(&mut self, name: &str) -> Option<&mut NetClass> {
        self.class_arr.iter_mut().find(|c| c.name == name)
    }

    /// Appends a new class with the given name to the collection and returns its index.
    pub fn append(
        &mut self,
        name: impl Into<String>,
        layer_structure: &LayerStructure,
        ignored_by_autorouter: bool,
    ) -> usize {
        let new_class = NetClass::new(name, layer_structure, ignored_by_autorouter);
        self.class_arr.push(new_class);
        self.class_arr.len() - 1
    }

    /// Appends a new class with an internally generated name ("class1", "class2", ...) and returns its index.
    pub fn append_with_generated_name(&mut self, layer_structure: &LayerStructure) -> usize {
        let mut index = 0;
        let name = loop {
            index += 1;
            let candidate = format!("class{index}");
            if self.get_by_name(&candidate).is_none() {
                break candidate;
            }
        };
        self.append(name, layer_structure, false)
    }

    /// Looks for a net class with uniform trace half-widths equal to `trace_half_width`,
    /// trace clearance class equal to `trace_clearance_class`, and via rule name equal to `via_rule_name`.
    pub fn find(
        &self,
        trace_half_width: i32,
        trace_clearance_class: usize,
        via_rule_name: Option<&str>,
    ) -> Option<&NetClass> {
        self.class_arr.iter().find(|c| {
            if c.get_trace_clearance_class() != trace_clearance_class {
                return false;
            }
            let via_match = match (c.get_via_rule(), via_rule_name) {
                (Some(vr), Some(name)) => vr.name == name,
                (None, None) => true,
                _ => false,
            };
            if !via_match {
                return false;
            }
            (0..c.layer_count()).all(|layer| c.get_trace_half_width(layer) == trace_half_width)
        })
    }

    /// Looks for a net class whose trace half-widths match `trace_half_width_arr`,
    /// trace clearance class matches `trace_clearance_class`, and via rule matches `via_rule_name`.
    pub fn find_by_widths(
        &self,
        trace_half_width_arr: &[i32],
        trace_clearance_class: usize,
        via_rule_name: Option<&str>,
    ) -> Option<&NetClass> {
        self.class_arr.iter().find(|c| {
            if c.get_trace_clearance_class() != trace_clearance_class
                || c.layer_count() != trace_half_width_arr.len()
            {
                return false;
            }
            let via_match = match (c.get_via_rule(), via_rule_name) {
                (Some(vr), Some(name)) => vr.name == name,
                (None, None) => true,
                _ => false,
            };
            if !via_match {
                return false;
            }
            (0..c.layer_count()).all(|layer| c.get_trace_half_width(layer) == trace_half_width_arr[layer])
        })
    }

    /// Removes the net class at the given index.
    pub fn remove(&mut self, index: usize) -> Option<NetClass> {
        if index < self.class_arr.len() {
            Some(self.class_arr.remove(index))
        } else {
            None
        }
    }

    /// Removes the net class with the given name. Returns false if not found.
    pub fn remove_by_name(&mut self, name: &str) -> bool {
        if let Some(pos) = self.class_arr.iter().position(|c| c.name == name) {
            self.class_arr.remove(pos);
            true
        } else {
            false
        }
    }

    /// Returns an iterator over the net classes.
    pub fn iter(&self) -> std::slice::Iter<'_, NetClass> {
        self.class_arr.iter()
    }

    /// Returns a mutable iterator over the net classes.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, NetClass> {
        self.class_arr.iter_mut()
    }
}

impl IntoIterator for NetClasses {
    type Item = NetClass;
    type IntoIter = std::vec::IntoIter<NetClass>;

    fn into_iter(self) -> Self::IntoIter {
        self.class_arr.into_iter()
    }
}

impl<'a> IntoIterator for &'a NetClasses {
    type Item = &'a NetClass;
    type IntoIter = std::slice::Iter<'a, NetClass>;

    fn into_iter(self) -> Self::IntoIter {
        self.class_arr.iter()
    }
}
