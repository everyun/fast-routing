//! Transactional undo/redo state manager for tracking board object changes.
//!
//! Ported from `app.freerouting.datastructures.UndoableObjects`.

use std::collections::BTreeMap;

/// Version node tracking an object's current state and undo/redo history.
#[derive(Debug, Clone)]
pub struct UndoableNode<V> {
    pub current: V,
    pub level: usize,
    pub undo_history: Vec<(usize, V)>,
    pub redo_history: Vec<(usize, V)>,
}

impl<V: Clone> UndoableNode<V> {
    pub fn new(value: V, level: usize) -> Self {
        UndoableNode {
            current: value,
            level,
            undo_history: Vec::new(),
            redo_history: Vec::new(),
        }
    }
}

/// Information about an object that was deleted at a given snapshot level.
#[derive(Debug, Clone)]
struct DeletedEntry<K, V> {
    key: K,
    node: UndoableNode<V>,
}

/// Transactional database of objects supporting multi-level snapshots, undo, redo, and commit.
#[derive(Debug, Clone)]
pub struct UndoableObjects<K: Clone + Ord, V: Clone> {
    objects: BTreeMap<K, UndoableNode<V>>,
    deleted_objects_stack: Vec<Vec<DeletedEntry<K, V>>>,
    created_objects_redo_stack: Vec<Vec<(K, UndoableNode<V>)>>,
    stack_level: usize,
    redo_possible: bool,
}

impl<K: Clone + Ord, V: Clone> Default for UndoableObjects<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Clone + Ord, V: Clone> UndoableObjects<K, V> {
    /// Creates a new `UndoableObjects` database at base level 0.
    pub fn new() -> Self {
        UndoableObjects {
            objects: BTreeMap::new(),
            deleted_objects_stack: Vec::new(),
            created_objects_redo_stack: Vec::new(),
            stack_level: 0,
            redo_possible: false,
        }
    }

    /// Returns the current transaction stack level.
    pub fn stack_level(&self) -> usize {
        self.stack_level
    }

    /// Returns `true` if a redo operation is currently available.
    pub fn is_redo_possible(&self) -> bool {
        self.redo_possible
    }

    /// Returns the number of currently active objects.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Returns `true` if there are no active objects.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Returns a reference to the active object for `key`, if present.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.objects.get(key).map(|node| &node.current)
    }

    /// Returns a mutable reference to the active object for `key`.
    ///
    /// Note: Call `save_for_undo` before modifying the object if undoability
    /// across the current snapshot boundary is desired.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.objects.get_mut(key).map(|node| &mut node.current)
    }

    /// Returns an iterator over all active `(key, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.objects.iter().map(|(k, node)| (k, &node.current))
    }

    /// Returns an iterator over all active values.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.objects.values().map(|node| &node.current)
    }

    /// Inserts a new object into the database at the current stack level.
    pub fn insert(&mut self, key: K, value: V) {
        self.disable_redo();
        let node = UndoableNode::new(value, self.stack_level);
        self.objects.insert(key, node);
    }

    /// Saves the current state of `key` for undo before it is modified.
    ///
    /// Must be called before an object is modified for the first time
    /// after a snapshot, if it existed before that snapshot.
    pub fn save_for_undo(&mut self, key: &K) {
        self.disable_redo();
        if let Some(node) = self.objects.get_mut(key) {
            if node.level < self.stack_level {
                let prev_state = node.current.clone();
                let prev_level = node.level;
                node.undo_history.push((prev_level, prev_state));
                node.level = self.stack_level;
            }
        }
    }

    /// Deletes an object from the database, recording it so it can be restored on undo.
    /// Returns `true` if the object was present and removed.
    pub fn delete(&mut self, key: &K) -> bool {
        self.disable_redo();
        if let Some(node) = self.objects.remove(key) {
            if !self.deleted_objects_stack.is_empty() {
                let delete_list = self.deleted_objects_stack.last_mut().unwrap();
                let recorded_node = if node.level < self.stack_level {
                    node
                } else if let Some(&(prev_lvl, ref prev_val)) = node.undo_history.last() {
                    let mut prev_node = UndoableNode::new(prev_val.clone(), prev_lvl);
                    prev_node.undo_history = node.undo_history[..node.undo_history.len() - 1].to_vec();
                    prev_node
                } else {
                    // Created on current level and deleted on current level: no prior undo state needed
                    return true;
                };
                delete_list.push(DeletedEntry {
                    key: key.clone(),
                    node: recorded_node,
                });
            }
            true
        } else {
            false
        }
    }

    /// Creates a new snapshot level, making subsequent mutations undoable.
    pub fn generate_snapshot(&mut self) {
        self.disable_redo();
        self.deleted_objects_stack.push(Vec::new());
        self.created_objects_redo_stack.push(Vec::new());
        self.stack_level += 1;
    }

    /// Reverts state to before the last snapshot.
    ///
    /// Appends cancelled (removed/reverted) objects to `cancelled_objects` and
    /// restored objects to `restored_objects`.
    /// Returns `false` if already at base level 0.
    pub fn undo(
        &mut self,
        cancelled_objects: &mut Vec<V>,
        restored_objects: &mut Vec<V>,
    ) -> bool {
        if self.stack_level == 0 {
            return false;
        }

        let curr_level = self.stack_level;
        let mut keys_to_remove = Vec::new();
        let mut created_for_redo = Vec::new();

        // 1. Process objects that were modified or created on this level
        for (k, node) in self.objects.iter_mut() {
            if node.level == curr_level {
                if let Some((prev_level, prev_val)) = node.undo_history.pop() {
                    let current_val = node.current.clone();
                    node.redo_history.push((curr_level, current_val.clone()));
                    cancelled_objects.push(current_val);
                    node.current = prev_val;
                    node.level = prev_level;
                    restored_objects.push(node.current.clone());
                } else {
                    // Object was created on this level (no undo history)
                    cancelled_objects.push(node.current.clone());
                    keys_to_remove.push(k.clone());
                    created_for_redo.push((k.clone(), node.clone()));
                }
            }
        }

        // Remove newly created objects from active map
        for k in keys_to_remove {
            self.objects.remove(&k);
        }

        // Store newly created objects in redo stack for this level
        if self.created_objects_redo_stack.len() >= curr_level {
            self.created_objects_redo_stack[curr_level - 1] = created_for_redo;
        }

        // 2. Restore deleted objects from deletedObjectsStack
        if self.deleted_objects_stack.len() >= curr_level {
            let delete_list = &self.deleted_objects_stack[curr_level - 1];
            for entry in delete_list {
                self.objects.insert(entry.key.clone(), entry.node.clone());
                restored_objects.push(entry.node.current.clone());
            }
        }

        self.stack_level -= 1;
        self.redo_possible = true;
        true
    }

    /// Re-applies changes that were previously undone.
    ///
    /// Appends cancelled objects to `cancelled_objects` and restored objects to `restored_objects`.
    /// Returns `false` if already at the top level or redo is unavailable.
    pub fn redo(
        &mut self,
        cancelled_objects: &mut Vec<V>,
        restored_objects: &mut Vec<V>,
    ) -> bool {
        if !self.redo_possible || self.stack_level >= self.deleted_objects_stack.len() {
            return false;
        }

        let target_level = self.stack_level + 1;

        // 1. Re-apply modifications
        for (_k, node) in self.objects.iter_mut() {
            if let Some(&(redo_lvl, _)) = node.redo_history.last() {
                if redo_lvl == target_level {
                    let (lvl, val) = node.redo_history.pop().unwrap();
                    cancelled_objects.push(node.current.clone());
                    node.undo_history.push((node.level, node.current.clone()));
                    node.current = val.clone();
                    node.level = lvl;
                    restored_objects.push(val);
                }
            }
        }

        // 2. Re-insert objects created on target level
        if self.created_objects_redo_stack.len() >= target_level {
            let created = self.created_objects_redo_stack[target_level - 1].clone();
            for (k, node) in created {
                restored_objects.push(node.current.clone());
                self.objects.insert(k, node);
            }
        }

        // 3. Re-delete objects that were deleted on target level
        if self.deleted_objects_stack.len() >= target_level {
            let delete_list = self.deleted_objects_stack[target_level - 1].clone();
            for entry in delete_list {
                if let Some(removed_node) = self.objects.remove(&entry.key) {
                    cancelled_objects.push(removed_node.current);
                }
            }
        }

        self.stack_level = target_level;
        true
    }

    /// Commits the top snapshot into the previous level without allowing undo back to that boundary.
    /// Returns `false` if already at base level 0.
    pub fn pop_snapshot(&mut self) -> bool {
        self.disable_redo();
        if self.stack_level == 0 {
            return false;
        }

        let curr_level = self.stack_level;

        // Adjust object levels
        for (_k, node) in self.objects.iter_mut() {
            if node.level == curr_level {
                node.level = curr_level - 1;
                // If there was an undo entry on curr_level - 1, collapse it
                if let Some(&(prev_lvl, _)) = node.undo_history.last() {
                    if prev_lvl == curr_level - 1 {
                        node.undo_history.pop();
                    }
                }
            }
        }

        // Merge deleted objects list into previous level
        let top_deleted = self.deleted_objects_stack.pop().unwrap_or_default();
        if let Some(prev_deleted) = self.deleted_objects_stack.last_mut() {
            for entry in top_deleted {
                let mut node = entry.node;
                if node.level == curr_level {
                    node.level = curr_level - 1;
                }
                prev_deleted.push(DeletedEntry {
                    key: entry.key,
                    node,
                });
            }
        }

        if !self.created_objects_redo_stack.is_empty() {
            self.created_objects_redo_stack.pop();
        }

        self.stack_level -= 1;
        true
    }

    /// Clears any redo history when new mutations are performed after an undo.
    fn disable_redo(&mut self) {
        if !self.redo_possible {
            return;
        }
        self.redo_possible = false;
        self.deleted_objects_stack.truncate(self.stack_level);
        self.created_objects_redo_stack.truncate(self.stack_level);
        for node in self.objects.values_mut() {
            node.redo_history.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MockItem {
        id: i32,
        name: String,
    }

    #[test]
    fn test_insert_and_get() {
        let mut db = UndoableObjects::new();
        db.insert(1, MockItem { id: 1, name: "Item1".into() });
        assert_eq!(db.len(), 1);
        assert_eq!(db.get(&1), Some(&MockItem { id: 1, name: "Item1".into() }));
    }

    #[test]
    fn test_undo_redo_insert() {
        let mut db = UndoableObjects::new();
        db.insert(1, MockItem { id: 1, name: "Base".into() });

        db.generate_snapshot();
        assert_eq!(db.stack_level(), 1);
        db.insert(2, MockItem { id: 2, name: "Created".into() });
        assert_eq!(db.len(), 2);

        let mut cancelled = Vec::new();
        let mut restored = Vec::new();
        assert!(db.undo(&mut cancelled, &mut restored));
        assert_eq!(db.len(), 1);
        assert_eq!(db.get(&2), None);
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].name, "Created");

        assert!(db.redo(&mut cancelled, &mut restored));
        assert_eq!(db.len(), 2);
        assert_eq!(db.get(&2), Some(&MockItem { id: 2, name: "Created".into() }));
    }

    #[test]
    fn test_undo_redo_modify() {
        let mut db = UndoableObjects::new();
        db.insert(1, MockItem { id: 1, name: "Original".into() });

        db.generate_snapshot();
        db.save_for_undo(&1);
        if let Some(item) = db.get_mut(&1) {
            item.name = "Modified".into();
        }
        assert_eq!(db.get(&1).unwrap().name, "Modified");

        let mut cancelled = Vec::new();
        let mut restored = Vec::new();
        assert!(db.undo(&mut cancelled, &mut restored));
        assert_eq!(db.get(&1).unwrap().name, "Original");

        assert!(db.redo(&mut cancelled, &mut restored));
        assert_eq!(db.get(&1).unwrap().name, "Modified");
    }

    #[test]
    fn test_undo_redo_delete() {
        let mut db = UndoableObjects::new();
        db.insert(1, MockItem { id: 1, name: "ToDelete".into() });

        db.generate_snapshot();
        assert!(db.delete(&1));
        assert_eq!(db.get(&1), None);

        let mut cancelled = Vec::new();
        let mut restored = Vec::new();
        assert!(db.undo(&mut cancelled, &mut restored));
        assert_eq!(db.get(&1), Some(&MockItem { id: 1, name: "ToDelete".into() }));

        assert!(db.redo(&mut cancelled, &mut restored));
        assert_eq!(db.get(&1), None);
    }

    #[test]
    fn test_pop_snapshot_commits() {
        let mut db = UndoableObjects::new();
        db.insert(1, MockItem { id: 1, name: "Original".into() });

        db.generate_snapshot();
        db.save_for_undo(&1);
        if let Some(item) = db.get_mut(&1) {
            item.name = "Modified".into();
        }
        assert!(db.pop_snapshot());
        assert_eq!(db.stack_level(), 0);

        let mut cancelled = Vec::new();
        let mut restored = Vec::new();
        assert!(!db.undo(&mut cancelled, &mut restored));
        assert_eq!(db.get(&1).unwrap().name, "Modified");
    }
}
