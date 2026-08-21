//! Information about a via definition (padstack, clearance class, drill-to-SMD).
//! Ported from `app.freerouting.rules.ViaInfo`.

use crate::padstack::Padstack;
use std::cmp::Ordering;

/// Information about a combination of a via padstack, via clearance class, and drill-to-SMD setting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ViaInfo {
    /// Name of the via definition.
    pub name: String,
    /// The padstack used by this via definition.
    pub padstack: Padstack,
    /// Clearance class index.
    pub clearance_class: usize,
    /// Whether this via may attach to an SMD pad.
    pub attach_smd_allowed: bool,
}

impl ViaInfo {
    /// Creates a new via definition.
    pub fn new(
        name: impl Into<String>,
        padstack: Padstack,
        clearance_class: usize,
        attach_smd_allowed: bool,
    ) -> Self {
        ViaInfo {
            name: name.into(),
            padstack,
            clearance_class,
            attach_smd_allowed,
        }
    }

    /// Returns the name of this via definition.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sets the name of this via definition.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Returns the padstack used by this via definition.
    pub fn padstack(&self) -> &Padstack {
        &self.padstack
    }

    /// Sets the padstack used by this via definition.
    pub fn set_padstack(&mut self, padstack: Padstack) {
        self.padstack = padstack;
    }

    /// Returns the clearance class used by this via definition.
    pub fn clearance_class(&self) -> usize {
        self.clearance_class
    }

    /// Sets the clearance class used by this via definition.
    pub fn set_clearance_class(&mut self, clearance_class: usize) {
        self.clearance_class = clearance_class;
    }

    /// Returns whether this via may attach to an SMD pad.
    pub fn attach_smd_allowed(&self) -> bool {
        self.attach_smd_allowed
    }

    /// Sets whether this via may attach to an SMD pad.
    pub fn set_attach_smd_allowed(&mut self, attach_smd_allowed: bool) {
        self.attach_smd_allowed = attach_smd_allowed;
    }
}

impl PartialOrd for ViaInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ViaInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}
