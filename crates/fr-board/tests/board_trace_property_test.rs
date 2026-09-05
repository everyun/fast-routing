//! Property tests for the board-level trace contract
//! (`v2-vs-v19-and-scope.md` §11.4-§11.6).
//!
//! Four properties are pinned:
//!
//! 1. **Split then combine is the identity.** Splitting a trace at a point on
//!    it and combining a piece back gives a trace with the *same corner
//!    multiset* — the split point is a collinear join, so the polyline
//!    constructor drops it again.
//! 2. **Normalize is idempotent.** `normalize_traces(net)` loops until a full
//!    pass changes nothing, so a second call must report `false` and leave the
//!    board bit-identical. If it did not, the 2000-iteration valve of §11.6
//!    would be doing real work on ordinary boards.
//! 3. **Rollback across split/combine is exact.** A transaction that splits,
//!    combines and normalizes, rolled back, restores a deep snapshot of the
//!    board exactly.
//! 4. **Pieces inherit everything.** Every piece of a split carries the
//!    original's net array, clearance class, layer, half width and fixed state.
//!
//! Traces are generated as walks on a coarse 45°-and-orthogonal grid, so that
//! segment midpoints are lattice points (a split anchor has to be one — see
//! `Board::split_trace_at_point`) and so that junctions between two traces
//! actually coincide exactly.

use fr_board::board::{Board, LayerStructure};
use fr_board::clearance::ClearanceTable;
use fr_board::item::{FixedState, Item, ItemId, ItemKind};
use fr_board::shape::{Padstack, TileShape};
use fr_board::NetNos;
use fr_geometry::planar::{IntBox, IntPoint, Point, Polyline};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u32) -> u32 {
        (self.next_u64() >> 32) as u32 % bound
    }
}

const LAYER_COUNT: usize = 2;
const STEP: i32 = 400;

fn new_board() -> Board {
    let mut board = Board::new(
        LayerStructure::signal_layers(LAYER_COUNT),
        ClearanceTable::uniform(3, LAYER_COUNT, 40),
    );
    board.padstacks_mut().push(Padstack::uniform(
        0,
        LAYER_COUNT as i32 - 1,
        TileShape::Box(IntBox::new(-60, -60, 60, 60)),
    ));
    board
}

/// A walk on the 8-neighbour grid: every segment is orthogonal or 45°, every
/// corner and every segment midpoint is a lattice point.
fn walk(rng: &mut Rng, corner_count: usize) -> Vec<IntPoint> {
    const DIRECTIONS: [(i32, i32); 8] = [
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
        (-1, -1),
        (0, -1),
        (1, -1),
    ];
    let mut corners = Vec::with_capacity(corner_count);
    let mut x = (rng.below(5) as i32 - 2) * STEP * 2;
    let mut y = (rng.below(5) as i32 - 2) * STEP * 2;
    corners.push(IntPoint::new(x, y));
    let mut last = usize::MAX;
    for _ in 1..corner_count {
        // Never repeat the previous direction: two collinear segments would be
        // collapsed by the polyline constructor and the walk would be shorter
        // than intended.
        let mut direction = rng.below(8) as usize;
        if direction == last {
            direction = (direction + 1) % 8;
        }
        last = direction;
        let (dx, dy) = DIRECTIONS[direction];
        let length = 1 + rng.below(3) as i32;
        x += dx * STEP * length;
        y += dy * STEP * length;
        corners.push(IntPoint::new(x, y));
    }
    corners
}

fn insert_walk(board: &mut Board, rng: &mut Rng, net: i32, layer: i32) -> Option<ItemId> {
    let corner_count = 2 + rng.below(4) as usize;
    let corners = walk(rng, corner_count);
    let polyline = Polyline::from_int_points(&corners);
    if polyline.corner_count() < 2 {
        return None;
    }
    board.insert_trace_without_cleaning(
        polyline,
        layer,
        50,
        NetNos::of(net),
        1,
        FixedState::Unfixed,
    )
}

fn corner_multiset(board: &Board, id: ItemId) -> Vec<(i32, i32)> {
    let ItemKind::Trace(trace) = &board.store().get(id).unwrap().kind else {
        panic!("not a trace");
    };
    let mut corners: Vec<(i32, i32)> = trace
        .polyline
        .corner_arr()
        .iter()
        .map(|corner| match corner {
            Point::Int(p) => (p.x, p.y),
            Point::Rational(_) => panic!("a grid walk must stay on the lattice"),
        })
        .collect();
    corners.sort_unstable();
    corners
}

fn snapshot(board: &Board) -> Vec<(ItemId, Item)> {
    board
        .store()
        .iter()
        .map(|(id, item)| (id, item.clone()))
        .collect()
}

fn traces(board: &Board) -> Vec<ItemId> {
    board
        .store()
        .iter()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Trace(_)))
        .map(|(id, _)| id)
        .collect()
}

/// A lattice midpoint of a random segment of `id`, if it has one.
fn segment_midpoint(board: &Board, id: ItemId, rng: &mut Rng) -> Option<IntPoint> {
    let ItemKind::Trace(trace) = &board.store().get(id)?.kind else {
        return None;
    };
    let corners = trace.polyline.corner_arr();
    if corners.len() < 2 {
        return None;
    }
    let index = rng.below(corners.len() as u32 - 1) as usize;
    let (Point::Int(a), Point::Int(b)) = (&corners[index], &corners[index + 1]) else {
        return None;
    };
    if (a.x + b.x) % 2 != 0 || (a.y + b.y) % 2 != 0 {
        return None;
    }
    Some(IntPoint::new((a.x + b.x) / 2, (a.y + b.y) / 2))
}

#[test]
fn split_then_combine_restores_the_same_corner_multiset() {
    let mut checked = 0;
    for seed in 1..=40u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut board = new_board();
        // Alone on the board, so the only contact at the split point is the
        // sibling piece and `combine` is unambiguous.
        let Some(id) = insert_walk(&mut board, &mut rng, 1, 0) else {
            continue;
        };
        let before = corner_multiset(&board, id);
        let Some(midpoint) = segment_midpoint(&board, id, &mut rng) else {
            continue;
        };

        let Some(pieces) = board.split_trace_at_point(id, &Point::Int(midpoint)) else {
            continue;
        };
        let pieces: Vec<ItemId> = pieces.into_iter().flatten().collect();
        assert_eq!(pieces.len(), 2, "seed {seed}: a midpoint split yields two");
        assert!(!board.store().contains(id));
        board.assert_consistent();

        assert!(
            board.combine_trace(pieces[0]),
            "seed {seed}: the two pieces must combine back"
        );
        let survivors = traces(&board);
        assert_eq!(survivors.len(), 1, "seed {seed}");
        assert_eq!(
            corner_multiset(&board, survivors[0]),
            before,
            "seed {seed}: split+combine changed the geometry"
        );
        board.assert_consistent();
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} usable cases were generated");
}

#[test]
fn split_pieces_inherit_every_attribute() {
    for seed in 1..=25u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let mut board = new_board();
        let corners = walk(&mut rng, 3);
        let Some(polyline) =
            Some(Polyline::from_int_points(&corners)).filter(|p| p.corner_count() >= 2)
        else {
            continue;
        };
        let nets = NetNos::from_slice(&[2, 3]);
        let Some(id) = board.insert_trace_without_cleaning(
            polyline,
            1,
            70,
            nets.clone(),
            2,
            FixedState::ShoveFixed,
        ) else {
            continue;
        };
        let Some(midpoint) = segment_midpoint(&board, id, &mut rng) else {
            continue;
        };
        let Some(pieces) = board.split_trace_at_point(id, &Point::Int(midpoint)) else {
            continue;
        };

        for piece in pieces.into_iter().flatten() {
            let item = board.store().get(piece).unwrap();
            assert_eq!(item.nets.as_slice(), nets.as_slice());
            assert_eq!(item.clearance_class, 2);
            assert_eq!(item.fixed_state, FixedState::ShoveFixed);
            let ItemKind::Trace(trace) = &item.kind else {
                panic!("a piece must be a trace")
            };
            assert_eq!(trace.layer, 1);
            assert_eq!(trace.half_width, 70);
        }
        board.assert_consistent();
    }
}

#[test]
fn normalize_is_idempotent() {
    for seed in 1..=30u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xA24B_AED4_963E_E407));
        let mut board = new_board();
        // Several traces on one net and one layer, so T-junctions and shared
        // corners are common and normalization has real work to do.
        for _ in 0..6 {
            insert_walk(&mut board, &mut rng, 1, 0);
        }

        board.normalize_traces(1);
        board.assert_consistent();
        assert!(
            !board.is_normalize_suppressed(1),
            "seed {seed}: an ordinary board must not trip the 2000-iteration valve"
        );

        let after_first = snapshot(&board);
        let changed_again = board.normalize_traces(1);
        assert!(
            !changed_again,
            "seed {seed}: the second normalize pass reported a change"
        );
        assert_eq!(
            snapshot(&board),
            after_first,
            "seed {seed}: the second normalize pass mutated the board"
        );
        board.assert_consistent();
    }
}

#[test]
fn normalize_of_a_single_trace_is_idempotent() {
    for seed in 1..=30u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let mut board = new_board();
        for _ in 0..4 {
            insert_walk(&mut board, &mut rng, 1, 0);
        }
        let ids = traces(&board);
        if ids.is_empty() {
            continue;
        }
        let target = ids[rng.below(ids.len() as u32) as usize];
        board.normalize_trace(target, None);

        // Normalizing every surviving trace once more must be a fixpoint.
        for id in traces(&board) {
            board.normalize_trace(id, None);
        }
        let stable = snapshot(&board);
        for id in traces(&board) {
            assert!(
                !board.normalize_trace(id, None),
                "seed {seed}: trace {id} still normalizes after a full pass"
            );
        }
        assert_eq!(snapshot(&board), stable, "seed {seed}");
        board.assert_consistent();
    }
}

#[test]
fn rollback_across_split_and_combine_restores_the_board_exactly() {
    for seed in 1..=25u64 {
        let mut rng = Rng::new(seed.wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
        let mut board = new_board();
        for _ in 0..5 {
            let net = 1 + rng.below(2) as i32;
            insert_walk(&mut board, &mut rng, net, 0);
        }
        let before = snapshot(&board);

        let mark = board.begin();
        for _ in 0..10 {
            let ids = traces(&board);
            if ids.is_empty() {
                break;
            }
            let target = ids[rng.below(ids.len() as u32) as usize];
            match rng.below(4) {
                0 => {
                    if let Some(midpoint) = segment_midpoint(&board, target, &mut rng) {
                        board.split_trace_at_point(target, &Point::Int(midpoint));
                    }
                }
                1 => {
                    board.combine_trace(target);
                }
                2 => {
                    board.normalize_trace(target, None);
                }
                _ => {
                    board.remove_item(target);
                }
            }
        }
        board.rollback_to(mark);

        assert_eq!(
            snapshot(&board),
            before,
            "seed {seed}: rollback did not restore the board"
        );
        board.assert_consistent();
    }
}

#[test]
fn combining_a_chain_is_order_independent() {
    // Whichever link the caller starts from, `combine()` must collapse the
    // whole chain — upstream's iterative loop walks both ends.
    for start in 0..5usize {
        let mut board = new_board();
        let mut ids = Vec::new();
        for i in 0..5i32 {
            ids.push(
                board
                    .insert_trace_without_cleaning(
                        Polyline::from_int_points(&[
                            IntPoint::new(i * 500, 0),
                            IntPoint::new((i + 1) * 500, 0),
                        ]),
                        0,
                        50,
                        NetNos::of(1),
                        1,
                        FixedState::Unfixed,
                    )
                    .unwrap(),
            );
        }
        assert!(board.combine_trace(ids[start]));
        assert_eq!(traces(&board).len(), 1, "starting from link {start}");
        let survivor = traces(&board)[0];
        assert_eq!(
            corner_multiset(&board, survivor),
            vec![(0, 0), (2500, 0)],
            "starting from link {start}"
        );
        board.assert_consistent();
    }
}
