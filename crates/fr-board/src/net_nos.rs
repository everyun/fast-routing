//! [`NetNos`] — the inline net-number array carried by every board item.
//!
//! Upstream every `Item` owns a `public int[] netNumbers` that is defensively
//! copied in the constructor (`v2-vs-v19-and-scope.md` §9.1). The array is
//! almost always length 1: only "tie" pins and the multi-subnet vias produced
//! by `Wiring.readViaScope` carry more, and
//! `RoutingBoard.reduceNetsOfRouteItems()` collapses most of those on load
//! (§10.5). A `Vec<i32>` per item would therefore be one heap allocation per
//! item to store four bytes.
//!
//! `NetNos` is a hand-rolled small-vector with [`INLINE_CAPACITY`] inline
//! slots — the workspace deliberately carries no `smallvec` dependency (see
//! the dependency list in the root `Cargo.toml`), and the API surface actually
//! needed here is tiny.
//!
//! Ordering is **insertion order, never sorted**: upstream's array order is
//! observable (`netNumbers[0]` is treated as "the" net by the SES writer and by
//! several router heuristics), so reordering would be a parity divergence.

use std::hash::{Hash, Hasher};
use std::ops::Deref;

/// Number of net numbers stored without spilling to the heap.
///
/// Three covers every item upstream produces in practice: one for ordinary
/// copper, two for a tie pin bridging two nets, three for the widest
/// multi-subnet via observed in the fixture suite.
pub const INLINE_CAPACITY: usize = 3;

/// The net numbers an item belongs to.
///
/// Net numbers are 1-based; `0` means "no net" and is never a real net (see
/// [`Item::contains_net`](crate::item::Item::contains_net), which rejects
/// `net_no <= 0` exactly like upstream `Item.containsNet`).
#[derive(Clone)]
pub enum NetNos {
    /// Up to [`INLINE_CAPACITY`] net numbers stored inline.
    Inline {
        /// Number of valid entries in `arr`.
        len: u8,
        /// The storage; entries at or past `len` are meaningless.
        arr: [i32; INLINE_CAPACITY],
    },
    /// Overflow storage for the rare item on more than [`INLINE_CAPACITY`] nets.
    Spilled(Vec<i32>),
}

impl NetNos {
    /// An empty net array (obstacle areas, board outlines).
    pub const EMPTY: NetNos = NetNos::Inline {
        len: 0,
        arr: [0; INLINE_CAPACITY],
    };

    /// Creates an empty net array.
    #[inline]
    pub const fn new() -> Self {
        NetNos::EMPTY
    }

    /// Creates a net array holding the single net `net_no`.
    #[inline]
    pub const fn of(net_no: i32) -> Self {
        NetNos::Inline {
            len: 1,
            arr: [net_no, 0, 0],
        }
    }

    /// Copies `nets` into a new net array, preserving order.
    pub fn from_slice(nets: &[i32]) -> Self {
        if nets.len() <= INLINE_CAPACITY {
            let mut arr = [0i32; INLINE_CAPACITY];
            arr[..nets.len()].copy_from_slice(nets);
            NetNos::Inline {
                len: nets.len() as u8,
                arr,
            }
        } else {
            NetNos::Spilled(nets.to_vec())
        }
    }

    /// The net numbers, in insertion order.
    #[inline]
    pub fn as_slice(&self) -> &[i32] {
        match self {
            NetNos::Inline { len, arr } => &arr[..*len as usize],
            NetNos::Spilled(v) => v.as_slice(),
        }
    }

    /// Number of nets.
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            NetNos::Inline { len, .. } => *len as usize,
            NetNos::Spilled(v) => v.len(),
        }
    }

    /// True when the item belongs to no net at all.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True when `net_no` is one of the stored numbers.
    ///
    /// This is the raw membership test; the upstream `containsNet` guard for
    /// `net_no <= 0` lives on [`Item`](crate::item::Item), not here.
    #[inline]
    pub fn contains(&self, net_no: i32) -> bool {
        self.as_slice().contains(&net_no)
    }

    /// True when the two arrays have at least one net number in common.
    ///
    /// Mirrors upstream `Item.sharesNet` — an `O(n·m)` scan over arrays that
    /// are essentially always length 1.
    #[inline]
    pub fn shares_any(&self, other: &NetNos) -> bool {
        let a = self.as_slice();
        let b = other.as_slice();
        a.iter().any(|n| b.contains(n))
    }

    /// Appends `net_no`, spilling to the heap past [`INLINE_CAPACITY`].
    ///
    /// Duplicates are *not* filtered — upstream does not filter them either,
    /// and the callers that must not produce them (the store's per-net index)
    /// deduplicate explicitly.
    pub fn push(&mut self, net_no: i32) {
        match self {
            NetNos::Inline { len, arr } => {
                if (*len as usize) < INLINE_CAPACITY {
                    arr[*len as usize] = net_no;
                    *len += 1;
                } else {
                    let mut v = Vec::with_capacity(INLINE_CAPACITY + 1);
                    v.extend_from_slice(arr);
                    v.push(net_no);
                    *self = NetNos::Spilled(v);
                }
            }
            NetNos::Spilled(v) => v.push(net_no),
        }
    }

    /// Removes the first occurrence of `net_no`; returns whether it was found.
    pub fn remove(&mut self, net_no: i32) -> bool {
        let Some(pos) = self.as_slice().iter().position(|n| *n == net_no) else {
            return false;
        };
        match self {
            NetNos::Inline { len, arr } => {
                arr.copy_within(pos + 1..*len as usize, pos);
                *len -= 1;
            }
            NetNos::Spilled(v) => {
                v.remove(pos);
            }
        }
        true
    }

    /// True when every net number is a real net (`> 0`).
    ///
    /// Upstream `Item.netsNormal()`; it gates trace splitting (§11.4) and the
    /// board-wide trace half-width bookkeeping (§11.1).
    #[inline]
    pub fn all_normal(&self) -> bool {
        !self.is_empty() && self.as_slice().iter().all(|n| *n > 0)
    }

    /// Iterates the net numbers in insertion order.
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, i32> {
        self.as_slice().iter()
    }

    /// Overwrites `self` with `src`, reusing the spilled allocation when both
    /// sides are spilled.
    ///
    /// Called from [`Item::clone_from`](crate::item::Item), which is what makes
    /// [`BoardHistory`](crate::history::BoardHistory) snapshots allocation-free
    /// in steady state.
    pub fn clone_from_ref(&mut self, src: &NetNos) {
        match (&mut *self, src) {
            (NetNos::Spilled(dst), NetNos::Spilled(s)) => {
                dst.clear();
                dst.extend_from_slice(s);
            }
            _ => *self = src.clone(),
        }
    }
}

impl Default for NetNos {
    #[inline]
    fn default() -> Self {
        NetNos::EMPTY
    }
}

impl Deref for NetNos {
    type Target = [i32];

    #[inline]
    fn deref(&self) -> &[i32] {
        self.as_slice()
    }
}

impl PartialEq for NetNos {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for NetNos {}

impl Hash for NetNos {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl std::fmt::Debug for NetNos {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl FromIterator<i32> for NetNos {
    fn from_iter<T: IntoIterator<Item = i32>>(iter: T) -> Self {
        let mut out = NetNos::new();
        for n in iter {
            out.push(n);
        }
        out
    }
}

impl<'a> IntoIterator for &'a NetNos {
    type Item = &'a i32;
    type IntoIter = std::slice::Iter<'a, i32>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_storage_does_not_allocate_for_the_common_case() {
        // The whole point of the type: a one-net item must fit inline, and the
        // enum must stay small enough to sit in an `Item` without bloating it.
        assert!(std::mem::size_of::<NetNos>() <= 32);
        let nets = NetNos::of(7);
        assert!(matches!(nets, NetNos::Inline { .. }));
        assert_eq!(nets.as_slice(), &[7]);
    }

    #[test]
    fn push_spills_past_inline_capacity_and_keeps_order() {
        let mut nets = NetNos::new();
        for n in 1..=(INLINE_CAPACITY as i32 + 2) {
            nets.push(n);
        }
        assert!(matches!(nets, NetNos::Spilled(_)));
        assert_eq!(nets.as_slice(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn inline_and_spilled_compare_and_hash_by_contents() {
        use std::collections::hash_map::DefaultHasher;

        let inline = NetNos::from_slice(&[1, 2, 3]);
        let spilled = NetNos::Spilled(vec![1, 2, 3]);
        assert_eq!(inline, spilled);

        let hash = |n: &NetNos| {
            let mut h = DefaultHasher::new();
            n.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&inline), hash(&spilled));
    }

    #[test]
    fn remove_preserves_the_order_of_the_survivors() {
        let mut nets = NetNos::from_slice(&[5, 9, 2]);
        assert!(nets.remove(9));
        assert_eq!(nets.as_slice(), &[5, 2]);
        assert!(!nets.remove(9));
    }

    #[test]
    fn all_normal_rejects_the_no_net_marker() {
        assert!(NetNos::of(1).all_normal());
        assert!(!NetNos::of(0).all_normal());
        assert!(!NetNos::EMPTY.all_normal());
    }

    #[test]
    fn shares_any_is_symmetric_membership() {
        let a = NetNos::from_slice(&[3, 4]);
        let b = NetNos::from_slice(&[4]);
        let c = NetNos::of(9);
        assert!(a.shares_any(&b));
        assert!(b.shares_any(&a));
        assert!(!a.shares_any(&c));
    }
}
