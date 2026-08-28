//! NxN clearance matrix describing spacing restrictions between clearance classes on layers.
//! Ported from `app.freerouting.rules.ClearanceMatrix` (release 2.3.0 @2d4de019).
//!
//! # Upstream quirks reproduced here (do not "fix" them)
//!
//! * **Storage normalization.** Every write clamps to `>= 0` and rounds odd values *up* to the
//!   next even integer, except `i32::MAX` which is decremented instead
//!   (`ClearanceMatrix.java:104-127`; `dsn-board-semantics.md` §3.4 "Clearance parity").
//!   Every stored clearance is therefore non-negative and even.
//! * **Asymmetric indexing.** Both the reader and the writer address the cube as
//!   `row[class_j].column[class_i].layer[layer]` — i.e. the *second* index selects the row.
//!   The setters are **directional**: [`ClearanceMatrix::set_value`] /
//!   [`ClearanceMatrix::set_value_on_layer`] write one cell only. Upstream's importer keeps the
//!   matrix symmetric by calling the setter twice with swapped arguments
//!   (`Structure.java:692-700`, `Network.java:672-679`), not by symmetrizing inside the setter.
//!   [`ClearanceMatrix::set_value_symmetric`] / [`ClearanceMatrix::set_value_on_layer_symmetric`]
//!   package that double call for importer use. A rewrite must keep the index order to stay
//!   bit-compatible with an externally written asymmetric matrix
//!   (`v2-vs-v19-and-scope.md` §14.2).
//! * **Out-of-range reads return 0.** `get_value` logs and returns 0 for any out-of-range class
//!   or layer index instead of failing (`ClearanceMatrix.java:132-148`). Listed as an explicitly
//!   replicated divergence in `docs/PARITY-DIVERGENCES.md`.
//! * **Max-value caches are monotonic.** `set_value` only ever raises `Row.max_value[layer]` and
//!   `max_value_on_layer[layer]` (`ClearanceMatrix.java:122-124`); lowering a cell leaves the
//!   caches stale-high. That is upstream behaviour and is *safe* for the search tree (it
//!   over-approximates the clearance halo), so it is preserved. Callers that need true maxima
//!   must opt in via [`ClearanceMatrix::recompute_max_values`], which is **not** upstream.

use crate::layer::LayerStructure;

/// Clearance safety margin added to clearances during routing when `add_safety_margin` is set.
///
/// `ClearanceMatrix.clearance_safety_margin = 16` (`ClearanceMatrix.java:17`); identical in v1.9.
/// It is an opt-in read-side addend — it is never stored (`v2-vs-v19-and-scope.md` §14.2).
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

    /// Creates a new instance with the 2 clearance classes `"null"` and `"default"` and
    /// initializes it with `default_value`.
    ///
    /// `ClearanceMatrix.get_default_instance` (`ClearanceMatrix.java:36-46`). Index 0 is the
    /// "no clearance" class (`BoardRules::clearance_class_none()`), index 1 is `"default"`
    /// (`BoardRules::default_clearance_class()`), which is also what the magic name `"wire"`
    /// aliases to in DSN clearance rules (`dsn-board-semantics.md` §7.8).
    pub fn default_instance(layer_structure: LayerStructure, default_value: i32) -> Self {
        let mut result = ClearanceMatrix::new(2, layer_structure, &["null", "default"]);
        result.set_default_value(default_value);
        result
    }

    /// Returns the number of the clearance class with the input name, or `None` if not found.
    ///
    /// Lookup is **case-insensitive** while [`Self::get_name`] returns the stored casing
    /// (`ClearanceMatrix.java:52-59`, `dsn-board-semantics.md` §7.8). Upstream returns `-1` for
    /// "not found"; several importer branches key off that (`< 0` ⇒ append the class).
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
    ///
    /// `ClearanceMatrix.set_default_value` (`ClearanceMatrix.java:70-86`). This writes **every**
    /// `(i, j)` with `i, j >= 1` and therefore wipes any previously set specific pair — which is
    /// why a PCB-level `(clearance v)` without a `(type …)` must be processed before the specific
    /// pairs (`dsn-board-semantics.md` §7.3).
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

    /// Sets the value of **one directed** entry of the clearance matrix on all layers.
    ///
    /// Mirrors `ClearanceMatrix.set_value(p_i, p_j, p_value)` (`ClearanceMatrix.java:91-96`):
    /// only the cell `row[class_j].column[class_i]` is written, so a single call leaves the
    /// matrix asymmetric. Use [`Self::set_value_symmetric`] to reproduce what the DSN importer
    /// does (`Structure.java:692-694`, `Network.java:672-674`: two calls with swapped indices).
    pub fn set_value(&mut self, class_i: usize, class_j: usize, value: i32) {
        for layer in 0..self.layer_structure.len() {
            self.set_value_on_layer(class_i, class_j, layer, value);
        }
    }

    /// Sets the value of **one directed** entry of the clearance matrix on a specific layer.
    ///
    /// Mirrors `ClearanceMatrix.set_value(p_i, p_j, p_layer, p_value)`
    /// (`ClearanceMatrix.java:109-127`).
    ///
    /// The value is normalized on store: clamped to `>= 0`, and rounded *up* to the next even
    /// integer when odd — except `i32::MAX`, which is decremented instead (rounding up would
    /// overflow). Only `row[class_j].column[class_i]` is written; see the module docs for the
    /// asymmetry.
    ///
    /// The two max-value caches are updated with `max()` only, exactly as upstream, so lowering
    /// a cell leaves them stale-high.
    ///
    /// Parity note: upstream is unguarded and throws `ArrayIndexOutOfBoundsException` on an
    /// out-of-range index. We silently ignore the write instead; no upstream caller can reach
    /// that state (`Structure.set_clearance_rule` always appends the class first).
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

    /// Sets both directions of a clearance class pair on all layers.
    ///
    /// This is the importer-level operation: upstream never symmetrizes inside the setter, it
    /// issues the two directed writes back to back
    /// (`Structure.java:692-694` for PCB/layer rules, `Network.java:672-674` for net-class rules).
    pub fn set_value_symmetric(&mut self, class_i: usize, class_j: usize, value: i32) {
        self.set_value(class_i, class_j, value);
        self.set_value(class_j, class_i, value);
    }

    /// Sets both directions of a clearance class pair on a specific layer.
    ///
    /// See [`Self::set_value_symmetric`]; upstream sites are `Structure.java:698-700` and
    /// `Network.java:676-679`.
    pub fn set_value_on_layer_symmetric(
        &mut self,
        class_i: usize,
        class_j: usize,
        layer: usize,
        value: i32,
    ) {
        self.set_value_on_layer(class_i, class_j, layer, value);
        self.set_value_on_layer(class_j, class_i, layer, value);
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
    ///
    /// Mirrors `ClearanceMatrix.get_value` (`ClearanceMatrix.java:132-164`). The lookup is
    /// **asymmetric**: it reads `row[class_j].column[class_i]`. Any out-of-range class or layer
    /// index yields `0` (upstream logs a trace line and returns 0 rather than throwing) — an
    /// explicitly replicated divergence, see `docs/PARITY-DIVERGENCES.md`.
    ///
    /// When `add_safety_margin` is true the stored value is returned plus
    /// [`CLEARANCE_SAFETY_MARGIN`]; the margin is a read-side opt-in and is never stored, so the
    /// result can be odd even though every stored value is even. The addition **saturates**; see
    /// the note at the call site.
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
            // Saturating, not wrapping. Upstream is `value + clearance_safety_margin` on a Java
            // `int`, which silently wraps to a large *negative* clearance for a class carrying
            // the "infinite clearance" idiom `Integer.MAX_VALUE` (stored as `MAX_VALUE - 1` by
            // the even-rounding in `set_value_on_layer`). A plain `+` would panic in debug builds
            // and reproduce the wrap in release; both are worse than clamping, and the divergence
            // is unreachable for any clearance below `i32::MAX - 16`.
            base_val.saturating_add(CLEARANCE_SAFETY_MARGIN)
        } else {
            base_val
        }
    }

    /// Returns the maximal required spacing of the given clearance class to all other clearance
    /// classes on `layer` (`ClearanceMatrix.max_value(p_i, p_layer)`, `ClearanceMatrix.java:169-175`).
    ///
    /// Both indices are clamped into range, as upstream does. The value comes from the monotonic
    /// cache described in the module docs, so after a clearance is *lowered* it can read higher
    /// than the true row maximum. That matches upstream and over-approximates, which is the safe
    /// direction for search-tree compensation.
    pub fn max_value_for_class(&self, class_i: usize, layer: usize) -> i32 {
        if self.class_count == 0 || self.layer_structure.is_empty() {
            return 0;
        }
        let i = class_i.min(self.class_count - 1);
        let layer_idx = layer.min(self.layer_structure.len() - 1);
        self.row[i].max_value[layer_idx]
    }

    /// Returns the maximum clearance value on the given layer across all classes
    /// (`ClearanceMatrix.max_value(p_layer)`, `ClearanceMatrix.java:177-181`).
    ///
    /// Same monotonic-cache caveat as [`Self::max_value_for_class`].
    pub fn max_value_on_layer(&self, layer: usize) -> i32 {
        if self.layer_structure.is_empty() {
            return 0;
        }
        let layer_idx = layer.min(self.layer_structure.len() - 1);
        self.max_value_on_layer[layer_idx]
    }

    /// Returns true if the values of the clearance matrix for (`class_i`, `class_j`) are not equal
    /// on all layers (`ClearanceMatrix.is_layer_dependent`, `ClearanceMatrix.java:186-194`).
    ///
    /// Like every accessor it reads `row[class_j].column[class_i]`, so the argument order is the
    /// same asymmetric one used by [`Self::get_value`]. Upstream is unguarded; we return `false`
    /// for out-of-range indices or an empty layer structure.
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
    ///
    /// `(self_clearance + 1) / 2` with integer (truncating) division — exactly
    /// `ClearanceMatrix.clearance_compensation_value` (`ClearanceMatrix.java:236-238`). Because
    /// every stored clearance is even, the `+ 1` never changes the result for stored values; it
    /// only matters if the cell was populated out of band. The search tree inflates every item by
    /// this amount, and `AutorouteControl.initNet` adds it to the trace half width
    /// (`compensatedTraceHalfWidth[i] = traceHalfWidth[i] + clearanceCompensationValue(traceCl, i)`,
    /// `v2-vs-v19-and-scope.md` §3.6). The safety margin is *not* included: the lookup passes
    /// `add_safety_margin = false`.
    pub fn clearance_compensation_value(&self, clearance_class: usize, layer: usize) -> i32 {
        (self.get_value(clearance_class, clearance_class, layer, false) + 1) / 2
    }

    /// Appends a new clearance class to the clearance matrix and initializes it with the values of
    /// the default class (index 1, `"default"`). Returns false if a clearance class with the given
    /// name already exists (name comparison is case-insensitive, as in [`Self::get_no`]).
    ///
    /// Mirrors `ClearanceMatrix.append_class` (`ClearanceMatrix.java:243-282`): the new row and
    /// column are seeded with `get_value(1, i, layer)` for every pre-existing class `i`, and the
    /// diagonal cell with `get_value(1, 1, layer)`. Pre-existing rows keep their max-value caches.
    ///
    /// Index stability matters: keepouts, pins and vias are resolved by clearance class *index*
    /// later in the import (`dsn-board-semantics.md` §7.3).
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
    ///
    /// Mirrors `ClearanceMatrix.remove_class` (`ClearanceMatrix.java:287-310`), including its
    /// cache quirk: upstream rebuilds every surviving row via `new Row(name)`, which allocates a
    /// **fresh zeroed** `max_value` array and never repopulates it, while the global
    /// `max_value_on_layer` is left untouched. So after a removal `max_value_for_class` reads 0
    /// for every class until the next write, and `max_value_on_layer` still reflects the
    /// pre-removal maximum. Call [`Self::recompute_max_values`] if you need true maxima.
    ///
    /// Parity note: upstream is unguarded and would throw for an out-of-range index; we no-op.
    /// The `class_count <= 1` guard is likewise ours — the only upstream caller
    /// (`BoardRules.remove_clearance_class`) refuses to remove a class that is still referenced.
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

        // Upstream parity: the surviving rows are reallocated, so their per-row max caches are
        // reset to zero and never refilled. `max_value_on_layer` deliberately keeps its old value.
        let layer_count = self.layer_structure.len();
        for row in &mut self.row {
            row.max_value = vec![0; layer_count];
        }
    }

    /// Recomputes the row and per-layer max-value caches from the stored cells.
    ///
    /// **Not upstream.** `ClearanceMatrix` only ever raises its caches (see the module docs), so
    /// they run stale-high after a value is lowered and stale-zero per row after
    /// [`Self::remove_class`]. This is the opt-in escape hatch for consumers that need exact
    /// maxima; the import path must not call it if bit-compatibility with the Java matrix matters.
    pub fn recompute_max_values(&mut self) {
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
