//! NxN clearance matrix describing spacing restrictions between clearance classes on layers.
//! Ported from `app.freerouting.rules.ClearanceMatrix`.

use crate::layer::LayerStructure;

/// Clearance safety margin added to clearances during routing.
pub const CLEARANCE_SAFETY_MARGIN: i32 = 16;

/// Represents a single entry of the clearance matrix for all layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClearanceMatrixEntry {
    /// Clearance value for each layer.
    pub layer: Vec<i32>,
}

impl ClearanceMatrixEntry {
    /// Creates a new matrix entry with all layer clearances set to 0.
    pub fn new(layer_count: usize) -> Self {
        ClearanceMatrixEntry {
            layer: vec![0; layer_count],
        }
    }

    /// Returns true if not all layer values are equal.
    pub fn is_layer_dependent(&self) -> bool {
        if self.layer.is_empty() {
            return false;
        }
        let first = self.layer[0];
        self.layer.iter().skip(1).any(|&v| v != first)
    }
}

/// Contains a row of entries of the clearance matrix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClearanceRow {
    /// Name of the clearance class for this row.
    pub name: String,
    /// Clearance values to each class (column index).
    pub column: Vec<ClearanceMatrixEntry>,
    /// Maximum clearance value for each layer in this row.
    pub max_value: Vec<i32>,
}

impl ClearanceRow {
    /// Creates a new `ClearanceRow`.
    pub fn new(name: impl Into<String>, class_count: usize, layer_count: usize) -> Self {
        ClearanceRow {
            name: name.into(),
            column: (0..class_count)
                .map(|_| ClearanceMatrixEntry::new(layer_count))
                .collect(),
            max_value: vec![0; layer_count],
        }
    }
}

/// NxN Matrix describing the spacing restrictions between N clearance classes on a fixed set of layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearanceMatrix {
    layer_structure: LayerStructure,
    max_value_on_layer: Vec<i32>,
    class_count: usize,
    row: Vec<ClearanceRow>,
}

impl ClearanceMatrix {
    /// Creates a new instance for `class_count` clearance classes on the layers in `layer_structure`.
    /// `name_arr` provides the name for each class.
    pub fn new(
        class_count: usize,
        layer_structure: LayerStructure,
        name_arr: &[impl AsRef<str>],
    ) -> Self {
        let count = class_count.max(1);
        let layer_count = layer_structure.len();
        let mut rows = Vec::with_capacity(count);

        for i in 0..count {
            let name = if i < name_arr.len() {
                name_arr[i].as_ref().to_string()
            } else {
                format!("class_{i}")
            };
            rows.push(ClearanceRow::new(name, count, layer_count));
        }

        ClearanceMatrix {
            layer_structure,
            max_value_on_layer: vec![0; layer_count],
            class_count: count,
            row: rows,
        }
    }

    /// Creates a new instance with the 2 clearance classes "null" and "default" and initializes it with `default_value`.
    pub fn default_instance(layer_structure: LayerStructure, default_value: i32) -> Self {
        let mut result = ClearanceMatrix::new(2, layer_structure, &["null", "default"]);
        result.set_default_value(default_value);
        result
    }

    /// Returns the number of the clearance class with the input name (case-insensitive), or `None` if not found.
    pub fn get_no(&self, name: &str) -> Option<usize> {
        self.row
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(name))
    }

    /// Gets the name of the clearance class with the input number.
    pub fn get_name(&self, clearance_class: usize) -> Option<&str> {
        self.row.get(clearance_class).map(|r| r.name.as_str())
    }

    /// Sets the value of all clearance classes with number >= 1 to `value` on all layers.
    pub fn set_default_value(&mut self, value: i32) {
        for layer in 0..self.layer_structure.len() {
            self.set_default_value_on_layer(layer, value);
        }
    }

    /// Sets the value of all clearance classes with number >= 1 to `value` on `layer`.
    pub fn set_default_value_on_layer(&mut self, layer: usize, value: i32) {
        for i in 1..self.class_count {
            for j in 1..self.class_count {
                self.set_value_on_layer(i, j, layer, value);
            }
        }
    }

    /// Sets the value of an entry in the clearance matrix to `value` on all layers.
    pub fn set_value(&mut self, class_i: usize, class_j: usize, value: i32) {
        for layer in 0..self.layer_structure.len() {
            self.set_value_on_layer(class_i, class_j, layer, value);
        }
    }

    /// Sets the value of an entry in the clearance matrix to `value` on a specific layer.
    /// Values are normalized to be non-negative and even.
    pub fn set_value_on_layer(
        &mut self,
        class_i: usize,
        class_j: usize,
        layer: usize,
        mut value: i32,
    ) {
        if class_i >= self.class_count || class_j >= self.class_count || layer >= self.layer_structure.len() {
            return;
        }

        // Assure that the clearance value is positive and even, and round it up if it is odd
        value = value.max(0);
        if value % 2 != 0 {
            if value == i32::MAX {
                value -= 1;
            } else {
                value += 1;
            }
        }

        self.row[class_j].column[class_i].layer[layer] = value;
        self.row[class_j].max_value[layer] = self.row[class_j].max_value[layer].max(value);
        self.max_value_on_layer[layer] = self.max_value_on_layer[layer].max(value);
    }

    /// Sets the value of an entry in the clearance matrix to `value` on all inner layers.
    pub fn set_inner_value(&mut self, class_i: usize, class_j: usize, value: i32) {
        let len = self.layer_structure.len();
        if len > 2 {
            for layer in 1..(len - 1) {
                self.set_value_on_layer(class_i, class_j, layer, value);
            }
        }
    }

    /// Gets the required spacing of clearance classes with index `class_i` and `class_j` on `layer`.
    pub fn get_value(
        &self,
        class_i: usize,
        class_j: usize,
        layer: usize,
        add_safety_margin: bool,
    ) -> i32 {
        if class_i >= self.class_count
            || class_j >= self.class_count
            || layer >= self.layer_structure.len()
        {
            return 0;
        }

        let base_val = self.row[class_j].column[class_i].layer[layer];
        if add_safety_margin {
            base_val + CLEARANCE_SAFETY_MARGIN
        } else {
            base_val
        }
    }

    /// Returns the maximal required spacing of the given clearance class to all other clearance classes on `layer`.
    pub fn max_value_for_class(&self, class_i: usize, layer: usize) -> i32 {
        if self.class_count == 0 || self.layer_structure.is_empty() {
            return 0;
        }
        let i = class_i.min(self.class_count - 1);
        let layer_idx = layer.min(self.layer_structure.len() - 1);
        self.row[i].max_value[layer_idx]
    }

    /// Returns the maximum clearance value on the given layer across all classes.
    pub fn max_value_on_layer(&self, layer: usize) -> i32 {
        if self.layer_structure.is_empty() {
            return 0;
        }
        let layer_idx = layer.min(self.layer_structure.len() - 1);
        self.max_value_on_layer[layer_idx]
    }

    /// Returns true if the values of the clearance matrix for (`class_i`, `class_j`) are not equal on all layers.
    pub fn is_layer_dependent(&self, class_i: usize, class_j: usize) -> bool {
        if class_i >= self.class_count || class_j >= self.class_count || self.layer_structure.is_empty() {
            return false;
        }
        self.row[class_j].column[class_i].is_layer_dependent()
    }

    /// Returns true if the values of the clearance matrix for (`class_i`, `class_j`) are not equal on all inner layers.
    pub fn is_inner_layer_dependent(&self, class_i: usize, class_j: usize) -> bool {
        let len = self.layer_structure.len();
        if len <= 2 || class_i >= self.class_count || class_j >= self.class_count {
            return false;
        }
        let compare = self.row[class_j].column[class_i].layer[1];
        for l in 2..(len - 1) {
            if self.row[class_j].column[class_i].layer[l] != compare {
                return true;
            }
        }
        false
    }

    /// Returns the row with the given index.
    pub fn get_row(&self, index: usize) -> Option<&ClearanceRow> {
        self.row.get(index)
    }

    /// Returns the number of clearance classes.
    pub fn class_count(&self) -> usize {
        self.class_count
    }

    /// Returns the layer count of this clearance matrix.
    pub fn layer_count(&self) -> usize {
        self.layer_structure.len()
    }

    /// Returns the underlying layer structure.
    pub fn layer_structure(&self) -> &LayerStructure {
        &self.layer_structure
    }

    /// Returns the clearance compensation value of the given class on the given layer.
    pub fn clearance_compensation_value(&self, clearance_class: usize, layer: usize) -> i32 {
        (self.get_value(clearance_class, clearance_class, layer, false) + 1) / 2
    }

    /// Appends a new clearance class to the clearance matrix and initializes it with the values of the default class.
    /// Returns false if a clearance class with the given name already exists.
    pub fn append_class(&mut self, class_name: &str) -> bool {
        if self.get_no(class_name).is_some() {
            return false;
        }

        let old_class_count = self.class_count;
        let layer_count = self.layer_structure.len();
        self.class_count += 1;

        // Append a matrix entry to each old row
        for i in 0..old_class_count {
            self.row[i].column.push(ClearanceMatrixEntry::new(layer_count));
        }

        // Append the new row
        self.row.push(ClearanceRow::new(class_name, self.class_count, layer_count));

        // Set the new matrix elements to default values
        for i in 0..old_class_count {
            for j in 0..layer_count {
                let default_val = self.get_value(1, i, j, false);
                self.set_value_on_layer(old_class_count, i, j, default_val);
                self.set_value_on_layer(i, old_class_count, j, default_val);
            }
        }

        for j in 0..layer_count {
            let default_val = self.get_value(1, 1, j, false);
            self.set_value_on_layer(old_class_count, old_class_count, j, default_val);
        }

        true
    }

    /// Removes the class with the given index from the clearance matrix.
    pub fn remove_class(&mut self, index: usize) {
        if index >= self.class_count || self.class_count <= 1 {
            return;
        }

        let old_class_count = self.class_count;
        self.class_count -= 1;

        // Remove the matrix entry with the given index from each old row
        for i in 0..old_class_count {
            if i < self.row.len() {
                self.row[i].column.remove(index);
            }
        }
        self.row.remove(index);

        // Recompute max values
        self.recompute_max_values();
    }

    /// Recomputes row and layer max values.
    fn recompute_max_values(&mut self) {
        let layer_count = self.layer_structure.len();
        self.max_value_on_layer = vec![0; layer_count];

        for row in &mut self.row {
            row.max_value = vec![0; layer_count];
            for col in &row.column {
                for l in 0..layer_count {
                    row.max_value[l] = row.max_value[l].max(col.layer[l]);
                }
            }
        }

        for l in 0..layer_count {
            for row in &self.row {
                self.max_value_on_layer[l] = self.max_value_on_layer[l].max(row.max_value[l]);
            }
        }
    }

    /// Returns true if all clearance values of the class with index `first` are equal to `second`.
    pub fn is_equal(&self, first: usize, second: usize) -> bool {
        if first == second {
            return true;
        }
        if first >= self.class_count || second >= self.class_count {
            return false;
        }
        let row1 = &self.row[first];
        let row2 = &self.row[second];
        for i in 1..self.class_count {
            if row1.column[i] != row2.column[i] {
                return false;
            }
        }
        true
    }
}
