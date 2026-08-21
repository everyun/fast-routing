//! Minimum-area spatial bounding tree for 2D intersection queries and pruning.
//!
//! Ported from `app.freerouting.datastructures.MinAreaTree`.

use crate::array_stack::ArrayStack;
use crate::shape_tree::{BoundingShape, LeafId};

/// Internal node representation in the arena pool.
#[derive(Debug, Clone)]
enum Node<S, T> {
    Inner {
        bounding_shape: S,
        parent: Option<usize>,
        first_child: usize,
        second_child: usize,
    },
    Leaf {
        bounding_shape: S,
        parent: Option<usize>,
        object: T,
        shape_index: usize,
    },
    Free {
        next_free: Option<usize>,
    },
}

impl<S, T> Node<S, T> {
    fn bounding_shape(&self) -> Option<&S> {
        match self {
            Node::Inner { bounding_shape, .. } => Some(bounding_shape),
            Node::Leaf { bounding_shape, .. } => Some(bounding_shape),
            Node::Free { .. } => None,
        }
    }

    fn parent(&self) -> Option<usize> {
        match self {
            Node::Inner { parent, .. } => *parent,
            Node::Leaf { parent, .. } => *parent,
            Node::Free { .. } => None,
        }
    }

    fn set_parent(&mut self, new_parent: Option<usize>) {
        match self {
            Node::Inner { parent, .. } => *parent = new_parent,
            Node::Leaf { parent, .. } => *parent = new_parent,
            Node::Free { .. } => {}
        }
    }
}

/// A spatial index tree organizing bounding shapes hierarchically by minimal area expansion.
#[derive(Debug, Clone)]
pub struct MinAreaTree<S: BoundingShape, T> {
    nodes: Vec<Node<S, T>>,
    free_head: Option<usize>,
    root: Option<usize>,
    leaf_count: usize,
}

impl<S: BoundingShape, T> Default for MinAreaTree<S, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: BoundingShape, T> MinAreaTree<S, T> {
    /// Creates an empty `MinAreaTree`.
    pub fn new() -> Self {
        MinAreaTree {
            nodes: Vec::new(),
            free_head: None,
            root: None,
            leaf_count: 0,
        }
    }

    /// Returns the number of items stored in the tree.
    pub fn len(&self) -> usize {
        self.leaf_count
    }

    /// Returns `true` if the tree contains no items.
    pub fn is_empty(&self) -> bool {
        self.leaf_count == 0
    }

    /// Clears all entries from the tree.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.free_head = None;
        self.root = None;
        self.leaf_count = 0;
    }

    fn allocate_node(&mut self, node: Node<S, T>) -> usize {
        if let Some(free_idx) = self.free_head {
            if let Node::Free { next_free } = self.nodes[free_idx] {
                self.free_head = next_free;
            }
            self.nodes[free_idx] = node;
            free_idx
        } else {
            let idx = self.nodes.len();
            self.nodes.push(node);
            idx
        }
    }

    fn free_node(&mut self, idx: usize) {
        self.nodes[idx] = Node::Free {
            next_free: self.free_head,
        };
        self.free_head = Some(idx);
    }

    /// Inserts an object with a bounding shape and optional shape index into the tree.
    pub fn insert(&mut self, bounding_shape: S, object: T, shape_index: usize) -> LeafId {
        self.leaf_count += 1;

        // Tree is empty: insert new leaf as root
        if self.root.is_none() {
            let leaf_idx = self.allocate_node(Node::Leaf {
                bounding_shape,
                parent: None,
                object,
                shape_index,
            });
            self.root = Some(leaf_idx);
            return LeafId(leaf_idx);
        }

        // Locate best leaf to pair with to minimize area expansion
        let mut curr = self.root.unwrap();
        loop {
            let (first_child, second_child) = match self.nodes[curr] {
                Node::Inner {
                    first_child,
                    second_child,
                    ..
                } => (first_child, second_child),
                Node::Leaf { .. } => break,
                Node::Free { .. } => unreachable!(),
            };

            let c1_bounds = self.nodes[first_child].bounding_shape().unwrap().clone();
            let c2_bounds = self.nodes[second_child].bounding_shape().unwrap().clone();

            let union1 = bounding_shape.union(&c1_bounds);
            let area_inc1 = union1.area() - c1_bounds.area();

            let union2 = bounding_shape.union(&c2_bounds);
            let area_inc2 = union2.area() - c2_bounds.area();

            if let Node::Inner {
                bounding_shape: ref mut curr_bounds,
                ..
            } = self.nodes[curr]
            {
                *curr_bounds = curr_bounds.union(&bounding_shape);
            }

            if area_inc1 <= area_inc2 {
                curr = first_child;
            } else {
                curr = second_child;
            }
        }

        let leaf_to_replace = curr;
        let leaf_to_replace_bounds = self.nodes[leaf_to_replace].bounding_shape().unwrap().clone();
        let leaf_to_replace_parent = self.nodes[leaf_to_replace].parent();

        let new_bounds = bounding_shape.union(&leaf_to_replace_bounds);

        let new_leaf_idx = self.allocate_node(Node::Leaf {
            bounding_shape,
            parent: None, // set below
            object,
            shape_index,
        });

        let new_inner_idx = self.allocate_node(Node::Inner {
            bounding_shape: new_bounds,
            parent: leaf_to_replace_parent,
            first_child: leaf_to_replace,
            second_child: new_leaf_idx,
        });

        if let Some(parent_idx) = leaf_to_replace_parent {
            if let Node::Inner {
                ref mut first_child,
                ref mut second_child,
                ..
            } = self.nodes[parent_idx]
            {
                if *first_child == leaf_to_replace {
                    *first_child = new_inner_idx;
                } else {
                    *second_child = new_inner_idx;
                }
            }
        }

        self.nodes[leaf_to_replace].set_parent(Some(new_inner_idx));
        self.nodes[new_leaf_idx].set_parent(Some(new_inner_idx));

        if self.root == Some(leaf_to_replace) {
            self.root = Some(new_inner_idx);
        }

        LeafId(new_leaf_idx)
    }

    /// Removes a leaf by its handle, returning the stored object.
    pub fn remove(&mut self, leaf_id: LeafId) -> Option<T> {
        let idx = leaf_id.0;
        if idx >= self.nodes.len() {
            return None;
        }

        let (object, parent_opt) = match self.nodes[idx] {
            Node::Leaf {
                parent,
                ref object,
                ..
            } => {
                let obj = unsafe { std::ptr::read(object as *const T) };
                (obj, parent)
            }
            _ => return None,
        };

        self.free_node(idx);
        self.leaf_count -= 1;

        let parent_idx = match parent_opt {
            Some(p) => p,
            None => {
                self.root = None;
                return Some(object);
            }
        };

        // Find the sibling leaf/node
        let (other_child, grand_parent) = match self.nodes[parent_idx] {
            Node::Inner {
                first_child,
                second_child,
                parent,
                ..
            } => {
                let sibling = if second_child == idx {
                    first_child
                } else {
                    second_child
                };
                (sibling, parent)
            }
            _ => unreachable!(),
        };

        self.nodes[other_child].set_parent(grand_parent);
        if let Some(gp_idx) = grand_parent {
            if let Node::Inner {
                ref mut first_child,
                ref mut second_child,
                ..
            } = self.nodes[gp_idx]
            {
                if *second_child == parent_idx {
                    *second_child = other_child;
                } else {
                    *first_child = other_child;
                }
            }
        } else {
            self.root = Some(other_child);
        }

        self.free_node(parent_idx);

        // Recalculate ancestors
        let mut node_to_recalc = grand_parent;
        while let Some(curr_idx) = node_to_recalc {
            let (new_bounds, parent) = match self.nodes[curr_idx] {
                Node::Inner {
                    first_child,
                    second_child,
                    ref bounding_shape,
                    parent,
                } => {
                    let b1 = self.nodes[first_child].bounding_shape().unwrap();
                    let b2 = self.nodes[second_child].bounding_shape().unwrap();
                    let nb = b1.union(b2);
                    if nb.contains(bounding_shape) {
                        break;
                    }
                    (nb, parent)
                }
                _ => break,
            };

            if let Node::Inner {
                ref mut bounding_shape,
                ..
            } = self.nodes[curr_idx]
            {
                *bounding_shape = new_bounds;
            }
            node_to_recalc = parent;
        }

        Some(object)
    }

    /// Queries all leaves whose bounding shapes overlap `query_shape`.
    pub fn overlaps(&self, query_shape: &S) -> Vec<(LeafId, &S, &T, usize)> {
        let mut results = Vec::new();
        let root_idx = match self.root {
            Some(r) => r,
            None => return results,
        };

        let mut stack = ArrayStack::new(64);
        stack.push(root_idx);

        while let Some(curr_idx) = stack.pop() {
            match self.nodes[curr_idx] {
                Node::Inner {
                    ref bounding_shape,
                    first_child,
                    second_child,
                    ..
                } => {
                    if bounding_shape.intersects(query_shape) {
                        stack.push(first_child);
                        stack.push(second_child);
                    }
                }
                Node::Leaf {
                    ref bounding_shape,
                    ref object,
                    shape_index,
                    ..
                } => {
                    if bounding_shape.intersects(query_shape) {
                        results.push((LeafId(curr_idx), bounding_shape, object, shape_index));
                    }
                }
                Node::Free { .. } => {}
            }
        }

        results
    }

    /// Returns an iterator over all live leaves in the tree.
    pub fn iter_leaves(&self) -> impl Iterator<Item = (LeafId, &S, &T, usize)> {
        self.nodes.iter().enumerate().filter_map(|(idx, node)| {
            if let Node::Leaf {
                bounding_shape,
                object,
                shape_index,
                ..
            } = node
            {
                Some((LeafId(idx), bounding_shape, object, *shape_index))
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape_tree::BoundingBox2D;

    #[test]
    fn test_tree_insert_query_remove() {
        let mut tree = MinAreaTree::new();
        assert!(tree.is_empty());

        let box1 = BoundingBox2D::new(0, 0, 10, 10);
        let box2 = BoundingBox2D::new(20, 20, 30, 30);
        let box3 = BoundingBox2D::new(5, 5, 15, 15);

        let id1 = tree.insert(box1, "Box1", 0);
        let id2 = tree.insert(box2, "Box2", 0);
        let id3 = tree.insert(box3, "Box3", 0);

        assert_eq!(tree.len(), 3);

        // Query overlapping with (0,0) -> (8,8) should hit Box1 and Box3
        let q1 = BoundingBox2D::new(0, 0, 8, 8);
        let hits = tree.overlaps(&q1);
        assert_eq!(hits.len(), 2);
        let hit_names: Vec<&str> = hits.iter().map(|(_, _, obj, _)| **obj).collect();
        assert!(hit_names.contains(&"Box1"));
        assert!(hit_names.contains(&"Box3"));

        // Query overlapping with (25,25) -> (26,26) should hit Box2 only
        let q2 = BoundingBox2D::new(25, 25, 26, 26);
        let hits2 = tree.overlaps(&q2);
        assert_eq!(hits2.len(), 1);
        assert_eq!(*hits2[0].2, "Box2");

        // Remove box1
        let removed = tree.remove(id1);
        assert_eq!(removed, Some("Box1"));
        assert_eq!(tree.len(), 2);

        let hits_after_remove = tree.overlaps(&q1);
        assert_eq!(hits_after_remove.len(), 1);
        assert_eq!(*hits_after_remove[0].2, "Box3");

        // Remove remaining
        tree.remove(id2);
        tree.remove(id3);
        assert!(tree.is_empty());
    }
}
