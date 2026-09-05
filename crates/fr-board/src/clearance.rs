//! Clearance values and the **compensation rule** every search tree is built
//! on (`search-tree-obstacles.md` §2.1, §7, §10).
//!
//! # The rule, in one place
//!
//! A *compensated* search tree of clearance class `T` stores every obstacle
//! shape pre-inflated, so that a routed object of class `T` can be treated as a
//! zero-width path with a fixed radius and **touching a stored shape is exactly
//! a clearance violation**. The split between the two halves is:
//!
//! ```text
//! obstacle inflation = clearance(item_cl, T, layer) − (clearance(T, T, layer) + 1) / 2
//! router radius      = half_width                  + (clearance(T, T, layer) + 1) / 2
//! ```
//!
//! Their sum is `half_width + clearance(item_cl, T, layer)` exactly — the real
//! requirement. Two details make the split lossless rather than approximately
//! right, and both are reproduced here:
//!
//! * **Clearance values are forced non-negative and even** when they are
//!   written ([`ClearanceTable::set_value_on_layer`], upstream
//!   `ClearanceMatrix.set_value:113-116`), so the halving is exact and the two
//!   halves really do add up.
//! * **Class 0 is never inflated.** It is upstream's "null" class, meaning "no
//!   clearance at all"; [`ClearanceSource::tree_compensation`] returns 0 for it
//!   on both sides of the query, which also makes an uncompensated tree
//!   (`compensated_class == 0`) fall out of the same expression instead of
//!   needing a branch.
//!
//! The inflation is clamped at 0: a tree class whose self-clearance exceeds its
//! clearance to the item would otherwise *shrink* obstacles, and upstream's
//! `Math.max(0, …)` (`ShapeSearchTree.java:77-86`) is what stops that.
//!
//! # Why a trait plus a table
//!
//! The authoritative clearance matrix is `fr_rules::ClearanceMatrix`, but
//! `fr-board` does not depend on `fr-rules` (see `ARCHITECTURE.md`'s crate
//! graph: rules are consumed *downstream* of the board today). Rather than pull
//! the dependency in and couple the two rewrites, the board owns a
//! [`ClearanceTable`] with the same semantics that the importer fills, and
//! everything that only *reads* clearances takes the [`ClearanceSource`] trait.
//! When `fr-board` grows the `fr-rules` dependency, the change is one
//! `impl ClearanceSource for fr_rules::ClearanceMatrix` and the table becomes a
//! test fixture; no call site moves.

/// Upstream `ClearanceMatrix.clearance_safety_margin` (`:21`), added by
/// `get_value(..., add_safety_margin = true)`.
///
/// Only the *uncompensated* sorted-clearance query and `BasicBoard.clearance_value`
/// use it; the compensated trees never do, which is why nothing in this crate
/// adds it yet.
pub const CLEARANCE_SAFETY_MARGIN: i32 = 16;

/// Read access to a clearance matrix, plus the compensation arithmetic derived
/// from it.
///
/// Implementors only have to supply [`value`](ClearanceSource::value) and the
/// two dimensions; everything else is the rule from the module docs and must
/// **not** be overridden — parity depends on every tree, every router radius
/// and every DRC probe computing it identically.
pub trait ClearanceSource {
    /// Number of clearance classes; class 0 is the "null" class.
    fn class_count(&self) -> usize;

    /// Number of layers the matrix is defined over.
    fn layer_count(&self) -> usize;

    /// Required spacing between `class_i` and `class_j` on `layer`, without the
    /// safety margin.
    ///
    /// Out-of-range indices return 0, matching upstream
    /// `ClearanceMatrix.get_value`.
    fn value(&self, class_i: i32, class_j: i32, layer: i32) -> i32;

    /// Upstream `ClearanceMatrix.get_value(class_i, class_j, layer, true)`: the
    /// pairwise clearance **with** the safety margin. Upstream adds the margin
    /// unconditionally for in-bounds indices — a zero matrix entry still
    /// becomes 16 (`ClearanceMatrix.java:137-151`) — and returns 0 only for
    /// out-of-range indices.
    #[inline]
    fn value_with_margin(&self, class_i: i32, class_j: i32, layer: i32) -> i32 {
        if class_i < 0
            || class_j < 0
            || layer < 0
            || class_i >= self.class_count() as i32
            || class_j >= self.class_count() as i32
            || layer >= self.layer_count() as i32
        {
            return 0;
        }
        self.value(class_i, class_j, layer) + CLEARANCE_SAFETY_MARGIN
    }

    /// Upstream `ClearanceMatrix.clearance_compensation_value(cl, layer)`
    /// (`:211-213`): half the class' self-clearance, rounded up.
    ///
    /// This is the amount the *routed* object grows by; see the module docs.
    #[inline]
    fn compensation_value(&self, clearance_class: i32, layer: i32) -> i32 {
        if clearance_class <= 0 {
            return 0;
        }
        (self.value(clearance_class, clearance_class, layer) + 1) / 2
    }

    /// The amount an obstacle of class `item_class` is inflated by in the tree
    /// compensated for `tree_class`, on `layer`
    /// (`ShapeSearchTree.clearance_compensation_value`, `:77-86`).
    ///
    /// Zero when either class is the null class, which covers the uncompensated
    /// default tree without a special case.
    #[inline]
    fn tree_compensation(&self, item_class: i32, tree_class: i32, layer: i32) -> i32 {
        if item_class <= 0 || tree_class <= 0 {
            return 0;
        }
        let required = self.value(item_class, tree_class, layer);
        (required - self.compensation_value(tree_class, layer)).max(0)
    }

    /// The radius a routed trace of `clearance_class` and `half_width` must be
    /// given when it is probed against the tree compensated for that class:
    /// `half_width + (self_clearance + 1) / 2`
    /// (`AutorouteControl.init_net`, `AutorouteControl.java:149-152`).
    #[inline]
    fn compensated_half_width(&self, half_width: i32, clearance_class: i32, layer: i32) -> i32 {
        half_width + self.compensation_value(clearance_class, layer)
    }
}

/// A dense clearance matrix owned by the board.
///
/// Same storage shape as upstream's `ClearanceMatrix` — `class × class × layer`
/// — with the same write-time normalization. Unlike upstream this crate stores
/// the matrix **symmetrically**: `set_value` writes both `(i, j)` and `(j, i)`,
/// because every reader in the router asks for an unordered pair and upstream's
/// importers always write both directions anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearanceTable {
    class_count: usize,
    layer_count: usize,
    /// `values[(j * class_count + i) * layer_count + layer]`.
    values: Vec<i32>,
}

impl ClearanceTable {
    /// A matrix of `class_count` classes over `layer_count` layers, all zero.
    ///
    /// Class 0 is the null class and stays zero unless a caller overwrites it —
    /// which it should not, see the module docs.
    pub fn new(class_count: usize, layer_count: usize) -> Self {
        ClearanceTable {
            class_count,
            layer_count,
            values: vec![0; class_count * class_count * layer_count.max(1)],
        }
    }

    /// A matrix where every pair of non-null classes clears `value` on every
    /// layer — the shape almost every fixture and test wants.
    pub fn uniform(class_count: usize, layer_count: usize, value: i32) -> Self {
        let mut table = ClearanceTable::new(class_count, layer_count);
        for i in 1..class_count as i32 {
            for j in 1..class_count as i32 {
                table.set_value(i, j, value);
            }
        }
        table
    }

    #[inline]
    fn index(&self, class_i: i32, class_j: i32, layer: i32) -> Option<usize> {
        let (i, j, l) = (class_i, class_j, layer);
        if i < 0 || j < 0 || l < 0 {
            return None;
        }
        let (i, j, l) = (i as usize, j as usize, l as usize);
        if i >= self.class_count || j >= self.class_count || l >= self.layer_count {
            return None;
        }
        Some((j * self.class_count + i) * self.layer_count + l)
    }

    /// Sets the clearance between two classes on every layer.
    pub fn set_value(&mut self, class_i: i32, class_j: i32, value: i32) {
        for layer in 0..self.layer_count as i32 {
            self.set_value_on_layer(class_i, class_j, layer, value);
        }
    }

    /// Sets the clearance between two classes on one layer.
    ///
    /// The value is clamped to `>= 0` and rounded **up to the next even
    /// number**, exactly as upstream `ClearanceMatrix.set_value:113-116` — this
    /// is what makes `(v + 1) / 2 + v / 2 == v` and therefore what makes the
    /// compensation split lossless.
    pub fn set_value_on_layer(&mut self, class_i: i32, class_j: i32, layer: i32, value: i32) {
        let mut value = value.max(0);
        if value % 2 != 0 {
            // Upstream `ClearanceMatrix.set_value:119-126` rounds odd values up
            // *except* `Integer.MAX_VALUE`, which it decrements — rounding that
            // one up would overflow, and the "every stored clearance is even"
            // invariant the compensation split depends on has to hold for it
            // too.
            if value == i32::MAX {
                value -= 1;
            } else {
                value += 1;
            }
        }
        if let Some(index) = self.index(class_i, class_j, layer) {
            self.values[index] = value;
        }
        if let Some(index) = self.index(class_j, class_i, layer) {
            self.values[index] = value;
        }
    }
}

impl ClearanceSource for ClearanceTable {
    #[inline]
    fn class_count(&self) -> usize {
        self.class_count
    }

    #[inline]
    fn layer_count(&self) -> usize {
        self.layer_count
    }

    #[inline]
    fn value(&self, class_i: i32, class_j: i32, layer: i32) -> i32 {
        match self.index(class_i, class_j, layer) {
            Some(index) => self.values[index],
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_values_are_clamped_and_rounded_up_to_even() {
        let mut table = ClearanceTable::new(3, 2);
        table.set_value(1, 2, 7);
        assert_eq!(table.value(1, 2, 0), 8);
        assert_eq!(table.value(2, 1, 1), 8, "the matrix is symmetric");

        table.set_value(1, 1, -5);
        assert_eq!(table.value(1, 1, 0), 0);

        // Upstream decrements `Integer.MAX_VALUE` rather than rounding it up,
        // so the stored value stays even and never wraps negative.
        table.set_value(2, 2, i32::MAX);
        assert_eq!(table.value(2, 2, 0), i32::MAX - 1);
        assert_eq!(
            table.value(2, 2, 0) % 2,
            0,
            "every stored clearance is even"
        );
    }

    #[test]
    fn the_two_halves_of_the_compensation_split_add_up() {
        // The load-bearing identity of §2.1: for every item class and tree
        // class, obstacle inflation + router radius == half_width + clearance.
        let mut table = ClearanceTable::new(4, 3);
        for i in 1..4 {
            for j in 1..4 {
                table.set_value(i, j, 10 * i + 6 * j);
            }
        }
        let half_width = 37;
        for item_cl in 1..4 {
            for tree_cl in 1..4 {
                for layer in 0..3 {
                    let inflation = table.tree_compensation(item_cl, tree_cl, layer);
                    let radius = table.compensated_half_width(half_width, tree_cl, layer);
                    assert_eq!(
                        inflation + radius,
                        half_width + table.value(item_cl, tree_cl, layer),
                        "split is lossy for item {item_cl} in tree {tree_cl} on layer {layer}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_null_class_is_never_inflated_on_either_side() {
        let table = ClearanceTable::uniform(3, 2, 40);
        assert_eq!(table.tree_compensation(0, 1, 0), 0, "null item class");
        assert_eq!(table.tree_compensation(1, 0, 0), 0, "uncompensated tree");
        assert_eq!(table.compensation_value(0, 0), 0);
        assert_eq!(table.compensated_half_width(50, 0, 0), 50);
    }

    #[test]
    fn inflation_never_goes_negative() {
        let mut table = ClearanceTable::new(3, 1);
        // A tree class that keeps a huge distance from itself but almost none
        // from the item class would give a negative inflation without the clamp.
        table.set_value(2, 2, 1000);
        table.set_value(1, 2, 10);
        assert_eq!(table.tree_compensation(1, 2, 0), 0);
    }

    #[test]
    fn out_of_range_lookups_are_zero_not_a_panic() {
        let table = ClearanceTable::uniform(2, 2, 20);
        assert_eq!(table.value(9, 1, 0), 0);
        assert_eq!(table.value(1, 1, 9), 0);
        assert_eq!(table.value(-1, 1, 0), 0);
    }
}
