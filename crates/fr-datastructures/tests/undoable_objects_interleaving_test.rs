//! `UndoableObjects` undo/redo round trip across every delete/insert/modify
//! interleaving inside one snapshot level.
//!
//! redo() must replay re-deletions before re-creations, otherwise a
//! delete(K) + insert(K, new) pair inside one snapshot redoes to "K absent".

use fr_datastructures::UndoableObjects;

type Db = UndoableObjects<i32, String>;

fn s(v: &str) -> String {
    v.to_string()
}

fn undo(db: &mut Db) -> bool {
    let (mut c, mut r) = (Vec::new(), Vec::new());
    db.undo(&mut c, &mut r)
}

fn redo(db: &mut Db) -> bool {
    let (mut c, mut r) = (Vec::new(), Vec::new());
    db.redo(&mut c, &mut r)
}

/// Exhaustively drive every op sequence of length <= 4 over a single key inside
/// one snapshot level, then check undo() restores the pre-level state and
/// redo() restores the post-level state.
#[test]
fn exhaustive_single_key_interleavings_round_trip() {
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Insert(u8),
        Delete,
        Modify(u8),
    }
    let ops = [
        Op::Insert(1),
        Op::Insert(2),
        Op::Delete,
        Op::Modify(1),
        Op::Modify(2),
    ];

    let mut cases = 0usize;
    for pre_exists in [false, true] {
        for len in 0..=4usize {
            let mut idx = vec![0usize; len];
            loop {
                let seq: Vec<Op> = idx.iter().map(|&i| ops[i]).collect();

                let mut db: Db = UndoableObjects::new();
                if pre_exists {
                    db.insert(0, s("base"));
                }
                let before = db.get(&0).cloned();

                db.generate_snapshot();
                for op in &seq {
                    match op {
                        Op::Insert(n) => {
                            // insert() requires the key to be absent.
                            if db.get(&0).is_none() {
                                db.insert(0, format!("new{n}"));
                            }
                        }
                        Op::Delete => {
                            db.delete(&0);
                        }
                        Op::Modify(n) => {
                            if db.get(&0).is_some() {
                                db.save_for_undo(&0);
                                *db.get_mut(&0).unwrap() = format!("mod{n}");
                            }
                        }
                    }
                }
                let after = db.get(&0).cloned();

                assert!(undo(&mut db), "undo failed for {pre_exists} {seq:?}");
                assert_eq!(
                    db.get(&0).cloned(),
                    before,
                    "undo mismatch, pre_exists={pre_exists} seq={seq:?}"
                );
                assert!(redo(&mut db), "redo failed for {pre_exists} {seq:?}");
                assert_eq!(
                    db.get(&0).cloned(),
                    after,
                    "redo mismatch, pre_exists={pre_exists} seq={seq:?}"
                );
                cases += 1;

                // odometer
                if len == 0 {
                    break;
                }
                let mut p = len;
                loop {
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                    idx[p] += 1;
                    if idx[p] < ops.len() {
                        break;
                    }
                    idx[p] = 0;
                    if p == 0 {
                        p = usize::MAX;
                        break;
                    }
                }
                if p == usize::MAX {
                    break;
                }
            }
        }
    }
    assert!(cases > 1000, "coverage {cases}");
    println!("checked {cases} interleavings");
}

/// Multiple keys deleted + recreated in the same level, plus keys touched in
/// only one of the two ways, must all redo correctly together.
#[test]
fn multi_key_delete_and_recreate_in_one_level() {
    let mut db: Db = UndoableObjects::new();
    db.insert(1, s("a-old"));
    db.insert(2, s("b-old"));
    db.insert(3, s("c-old"));

    db.generate_snapshot();
    db.delete(&1);
    db.insert(1, s("a-new")); // delete + recreate
    db.delete(&2); // pure delete
    db.save_for_undo(&3);
    *db.get_mut(&3).unwrap() = s("c-mod"); // pure modify
    db.insert(4, s("d-new")); // pure create

    assert!(undo(&mut db));
    assert_eq!(db.get(&1), Some(&s("a-old")));
    assert_eq!(db.get(&2), Some(&s("b-old")));
    assert_eq!(db.get(&3), Some(&s("c-old")));
    assert_eq!(db.get(&4), None);

    assert!(redo(&mut db));
    assert_eq!(db.get(&1), Some(&s("a-new")));
    assert_eq!(db.get(&2), None);
    assert_eq!(db.get(&3), Some(&s("c-mod")));
    assert_eq!(db.get(&4), Some(&s("d-new")));
}

/// Two nested snapshot levels, undone and redone one at a time.
#[test]
fn nested_levels_undo_redo_stepwise() {
    let mut db: Db = UndoableObjects::new();
    db.insert(1, s("v0"));

    db.generate_snapshot(); // level 1
    db.delete(&1);
    db.insert(1, s("v1"));

    db.generate_snapshot(); // level 2
    db.delete(&1);
    db.insert(1, s("v2"));

    assert_eq!(db.get(&1), Some(&s("v2")));
    assert!(undo(&mut db));
    assert_eq!(db.get(&1), Some(&s("v1")), "undo to level 1");
    assert!(undo(&mut db));
    assert_eq!(db.get(&1), Some(&s("v0")), "undo to level 0");
    assert!(redo(&mut db));
    assert_eq!(db.get(&1), Some(&s("v1")), "redo to level 1");
    assert!(redo(&mut db));
    assert_eq!(db.get(&1), Some(&s("v2")), "redo to level 2");
}

/// Repeated undo/redo of the same level must be stable.
#[test]
fn repeated_undo_redo_is_stable() {
    let mut db: Db = UndoableObjects::new();
    db.insert(1, s("old"));
    db.generate_snapshot();
    db.delete(&1);
    db.insert(1, s("new"));

    for round in 0..5 {
        assert!(undo(&mut db), "round {round}");
        assert_eq!(db.get(&1), Some(&s("old")), "round {round}");
        assert!(redo(&mut db), "round {round}");
        assert_eq!(db.get(&1), Some(&s("new")), "round {round}");
    }
}

/// pop_snapshot() after a delete + recreate in the same level must keep the
/// recreated value and drop the level.
#[test]
fn pop_snapshot_after_delete_and_recreate() {
    let mut db: Db = UndoableObjects::new();
    db.insert(1, s("old"));
    db.generate_snapshot();
    db.delete(&1);
    db.insert(1, s("new"));
    assert!(db.pop_snapshot());
    assert_eq!(db.get(&1), Some(&s("new")));
    assert_eq!(db.stack_level(), 0);
}
