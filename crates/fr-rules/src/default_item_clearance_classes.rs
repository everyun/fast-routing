//! Stores the default clearance class for each item type.
//! Ported from `app.freerouting.rules.DefaultItemClearanceClasses`.

/// Defines the item classes for which default clearance values are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemClass {
    None = 0,
    Trace = 1,
    Via = 2,
    Pin = 3,
    Smd = 4,
    Area = 5,
}

impl ItemClass {
    /// Total count of item classes.
    pub const COUNT: usize = 6;

    /// All item classes.
    pub const ALL: [ItemClass; 6] = [
        ItemClass::None,
        ItemClass::Trace,
        ItemClass::Via,
        ItemClass::Pin,
        ItemClass::Smd,
        ItemClass::Area,
    ];

    /// Returns the ordinal index (0..5).
    pub fn ordinal(self) -> usize {
        self as usize
    }

    /// Converts an ordinal index into an `ItemClass`.
    pub fn from_ordinal(index: usize) -> Option<ItemClass> {
        match index {
            0 => Some(ItemClass::None),
            1 => Some(ItemClass::Trace),
            2 => Some(ItemClass::Via),
            3 => Some(ItemClass::Pin),
            4 => Some(ItemClass::Smd),
            5 => Some(ItemClass::Area),
            _ => None,
        }
    }
}

/// Stores the default clearance class for each item type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DefaultItemClearanceClasses {
    arr: [i32; ItemClass::COUNT],
}

impl Default for DefaultItemClearanceClasses {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultItemClearanceClasses {
    /// Creates a new instance of `DefaultItemClearanceClasses`.
    ///
    /// Following Java semantics: `None` (index 0) remains 0, and all other
    /// item classes (indices 1..5) are initialized to 1.
    pub fn new() -> Self {
        let mut res = DefaultItemClearanceClasses {
            arr: [0; ItemClass::COUNT],
        };
        res.set_all(1);
        res
    }

    /// Returns the number of the default clearance class for the input item class.
    pub fn get(&self, item_class: ItemClass) -> i32 {
        self.arr[item_class.ordinal()]
    }

    /// Sets the index of the default clearance class of the input item class to `index`.
    pub fn set(&mut self, item_class: ItemClass, index: i32) {
        self.arr[item_class.ordinal()] = index;
    }

    /// Sets the indices of all default item clearance classes (except `None`) to `index`.
    pub fn set_all(&mut self, index: i32) {
        for i in 1..self.arr.len() {
            self.arr[i] = index;
        }
    }
}
