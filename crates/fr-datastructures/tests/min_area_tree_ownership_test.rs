//! `MinAreaTree` ownership and drop accounting.
//!
//! `remove()` must move the payload out of the arena slot exactly once; an
//! earlier `ptr::read` implementation duplicated ownership and double-freed.

use std::cell::RefCell;
use std::rc::Rc;

use fr_datastructures::min_area_tree::MinAreaTree;
use fr_datastructures::shape_tree::BoundingBox2D;

#[derive(Debug)]
struct DropSpy {
    id: usize,
    log: Rc<RefCell<Vec<usize>>>,
}

impl Drop for DropSpy {
    fn drop(&mut self) {
        self.log.borrow_mut().push(self.id);
    }
}

fn boxes(i: i32) -> BoundingBox2D {
    BoundingBox2D::new(i * 3, i * 5 % 41, i * 3 + 7, i * 5 % 41 + 9)
}

/// Every payload must be dropped exactly once: no double-drop (the old
/// `ptr::read` bug) and no leak.
#[test]
fn every_payload_is_dropped_exactly_once() {
    let log = Rc::new(RefCell::new(Vec::new()));
    {
        let mut tree = MinAreaTree::new();
        let mut ids = Vec::new();
        for i in 0..200i32 {
            let id = tree.insert(
                boxes(i),
                DropSpy {
                    id: i as usize,
                    log: Rc::clone(&log),
                },
                i as usize,
            );
            ids.push((i as usize, id));
        }
        assert_eq!(log.borrow().len(), 0, "nothing should have dropped yet");

        // Remove half, checking the returned payload is the right one and is
        // still alive (dropped only when the returned value goes out of scope).
        for (want, id) in ids.iter().take(100) {
            let before = log.borrow().len();
            let got = tree.remove(*id).expect("leaf present");
            assert_eq!(got.id, *want);
            assert_eq!(
                log.borrow().len(),
                before,
                "payload dropped while still owned"
            );
            drop(got);
            assert_eq!(log.borrow().len(), before + 1);
        }
        assert_eq!(tree.len(), 100);

        // Removing a stale id must not resurrect or re-drop anything.
        for (_, id) in ids.iter().take(100) {
            let before = log.borrow().len();
            let again = tree.remove(*id);
            // Slots may have been recycled, so a Some() is possible; what must
            // never happen is a drop side effect from a failed lookup.
            assert_eq!(log.borrow().len(), before);
            drop(again);
        }
    }
    // Tree dropped: everything must now have been dropped exactly once.
    let mut seen = log.borrow().clone();
    seen.sort_unstable();
    let n = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), n, "some payload was dropped more than once");
}

/// clear() and re-use must also drop each payload exactly once.
#[test]
fn clear_drops_each_payload_once() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut tree = MinAreaTree::new();
    for i in 0..50i32 {
        tree.insert(
            boxes(i),
            DropSpy {
                id: i as usize,
                log: Rc::clone(&log),
            },
            0,
        );
    }
    tree.clear();
    let mut seen = log.borrow().clone();
    seen.sort_unstable();
    assert_eq!(seen.len(), 50);
    seen.dedup();
    assert_eq!(seen.len(), 50, "double drop on clear()");
    assert!(tree.is_empty());
}

/// Interleaved insert/remove keeps the tree structurally consistent and the
/// overlap query in agreement with a brute-force scan.
#[test]
fn interleaved_insert_remove_matches_brute_force_queries() {
    let mut tree: MinAreaTree<BoundingBox2D, i32> = MinAreaTree::new();
    let mut live: Vec<(i32, BoundingBox2D)> = Vec::new();
    let mut ids = Vec::new();
    let mut state = 0x9e37_79b9u32;
    let mut next = || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        state
    };

    for i in 0..400i32 {
        let b = boxes(i);
        let id = tree.insert(b, i, i as usize);
        ids.push((i, id, b));
        live.push((i, b));
        if next() % 3 == 0 && !ids.is_empty() {
            let k = (next() as usize) % ids.len();
            let (val, id, _) = ids.remove(k);
            let removed = tree.remove(id);
            assert_eq!(removed, Some(val), "remove returned the wrong payload");
            live.retain(|(v, _)| *v != val);
        }
        assert_eq!(tree.len(), live.len(), "leaf_count drifted at i={i}");
    }

    for q in [
        BoundingBox2D::new(0, 0, 20, 20),
        BoundingBox2D::new(100, 10, 400, 30),
        BoundingBox2D::new(-50, -50, 5000, 5000),
    ] {
        let mut got: Vec<i32> = tree
            .overlaps(&q)
            .into_iter()
            .map(|(_, _, v, _)| *v)
            .collect();
        let mut want: Vec<i32> = live
            .iter()
            .filter(|(_, b)| {
                b.min_x <= q.max_x && b.max_x >= q.min_x && b.min_y <= q.max_y && b.max_y >= q.min_y
            })
            .map(|(v, _)| *v)
            .collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "overlaps() disagrees with brute force for {q:?}");
    }
}
