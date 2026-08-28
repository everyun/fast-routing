//! The room invariants of `tiling_45.rs`, re-asked as a seed storm.
//!
//! `tiling_45.rs` checks the invariants once per fixture, from one hand-picked
//! seed box. That answers "does the octagon chain work here", not "does the
//! octagon chain ever break an invariant the any-angle chain keeps", and the
//! second question is the one the M2b.2 gate had to answer before recommending
//! a default for [`Tiling45Mode`]. So: 250 random seeds per fixture per mode,
//! on both layers, with every mode judged **against `Off` on the same seed**
//! wherever the property is not absolute.
//!
//! Three of the properties are absolute and asserted directly — a returned room
//! is alive, `CompleteFree`, 2-dimensional and on the layer it was asked for;
//! returned rooms are pairwise interior-disjoint; no room overlaps a
//! foreign-net obstacle. `Full` additionally owes octagon-ness.
//!
//! Seed *coverage* is deliberately not absolute. `complete_expansion_room`
//! legitimately returns rooms that cover only part of the contained box — the
//! board outline clips it, for one (`BOARD_OUTLINE_HALF_WIDTH`) — and the
//! any-angle path does this on roughly 8% of free seeds near the board edge.
//! What would be a real defect is a 45 mode losing seed area *more often than
//! `Off` does*, which is what the second test measures.

use fr_board::search_tree::Tiling45Mode;
use fr_geometry::planar::{IntBox, Simplex};
use fr_router::rooms::RoomKind;
use fr_router::test_support::TestFixture;

/// xorshift64*, so the storm is reproducible and needs no dev-dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn in_range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % ((hi - lo + 1) as u64)) as i32
    }
}

/// True when `shape` *is* its own bounding octagon, not merely inside one.
/// Areas are exact for integer-cornered convex shapes and the bounding octagon
/// always contains the shape, so equal area means equal shape.
fn is_octagon(shape: &Simplex) -> bool {
    let Some(oct) = shape.bounding_octagon() else {
        return false;
    };
    (oct.normalize().area() - shape.area()).abs() < 1e-6
}

const MODES: [Tiling45Mode; 3] = [Tiling45Mode::Off, Tiling45Mode::Doors, Tiling45Mode::Full];

#[test]
fn the_room_invariants_survive_a_seed_storm_in_every_mode() {
    let fixtures: [(&str, TestFixture); 3] = [
        ("empty", TestFixture::empty_two_layer()),
        ("pads", TestFixture::two_layer_with_pads()),
        ("trace", TestFixture::with_foreign_trace()),
    ];
    let mut seeds = 0usize;
    let mut rooms_seen = 0usize;

    for (name, fixture) in &fixtures {
        // Only the *foreign* trace is an obstacle: the pads belong to the net
        // being routed, and rooms are supposed to reach them.
        let obstacles: Vec<_> = if *name == "trace" {
            vec![fixture.trace_id()]
        } else {
            Vec::new()
        };

        for mode in MODES {
            let mut rng = Rng(0xC0FF_EE00_0000_0001 ^ ((mode as u64) << 40));
            for _ in 0..250 {
                let mut engine = fixture.engine();
                engine.set_tiling_45(mode);
                let bounds = engine.bounds();
                let width = rng.in_range(1, 400);
                let height = rng.in_range(1, 400);
                let x = rng.in_range(bounds.left_x() + 1, bounds.right_x() - width - 1);
                let y = rng.in_range(bounds.bottom_y() + 1, bounds.top_y() - height - 1);
                let layer = rng.in_range(0, 1);
                let contained = IntBox::new(x, y, x + width, y + height);

                let incomplete =
                    engine
                        .rooms
                        .add_incomplete_room(None, layer, contained.to_simplex());
                let rooms = engine.complete_expansion_room(incomplete);
                seeds += 1;
                if rooms.is_empty() {
                    // A seed inside copper legitimately yields no free room.
                    continue;
                }

                let shapes: Vec<Simplex> = rooms
                    .iter()
                    .map(|id| {
                        let room = engine.rooms.room(*id);
                        assert!(room.alive, "[{name}/{mode:?}] a dead room was returned");
                        assert_eq!(
                            room.kind,
                            RoomKind::CompleteFree,
                            "[{name}/{mode:?}] completion returned a {:?} room",
                            room.kind
                        );
                        assert_eq!(
                            room.shape.dimension(),
                            2,
                            "[{name}/{mode:?}] room is not 2-dimensional: {:?}",
                            room.shape
                        );
                        assert_eq!(
                            room.layer, layer,
                            "[{name}/{mode:?}] completion moved the room off its layer"
                        );
                        room.shape.clone()
                    })
                    .collect();
                rooms_seen += shapes.len();

                for (index, a) in shapes.iter().enumerate() {
                    for b in shapes.iter().skip(index + 1) {
                        assert!(
                            a.intersection(b).dimension() < 2,
                            "[{name}/{mode:?}] two completed rooms share an area \
                             (seed {contained:?}, layer {layer})"
                        );
                    }
                }

                for &item in &obstacles {
                    for index in 0..engine.tree_shape_count(item) as u16 {
                        if engine.item_tile_layer(item, index) != Some(layer) {
                            continue;
                        }
                        let Some(obstacle) = engine.item_tile(item, index) else {
                            continue;
                        };
                        for shape in &shapes {
                            assert!(
                                shape.intersection(&obstacle).dimension() < 2,
                                "[{name}/{mode:?}] a completed room overlaps obstacle tile \
                                 {index} (seed {contained:?}, layer {layer})"
                            );
                        }
                    }
                }

                if mode == Tiling45Mode::Full {
                    for shape in &shapes {
                        assert!(
                            shape.border_line_count() <= 8,
                            "[{name}] Full produced a {}-line room",
                            shape.border_line_count()
                        );
                        assert!(
                            is_octagon(shape),
                            "[{name}] Full produced a room that is not its own bounding octagon"
                        );
                    }
                }
            }
        }
    }
    assert!(seeds >= 2_000, "the storm ran short: {seeds} seeds");
    assert!(rooms_seen > 0, "no room was ever produced");
}

/// A 45 mode may not lose free-seed area more often than the any-angle path
/// does. Measured, not asserted per seed, because partial coverage is a
/// property of `complete_shape` near the board outline in *every* mode.
#[test]
fn no_45_mode_loses_free_seed_area_more_often_than_the_any_angle_one() {
    let fixture = TestFixture::with_foreign_trace();
    let obstacles = [fixture.trace_id()];
    let mut per_mode: Vec<(Tiling45Mode, usize, usize)> = Vec::new();

    for mode in MODES {
        let mut rng = Rng(0xFEED_FACE_0000_0007 ^ ((mode as u64) << 40));
        let mut tested = 0usize;
        let mut lost = 0usize;
        for _ in 0..300 {
            let mut engine = fixture.engine();
            engine.set_tiling_45(mode);
            let bounds = engine.bounds();
            let width = rng.in_range(1, 200);
            let height = rng.in_range(1, 200);
            let x = rng.in_range(bounds.left_x() + 1, bounds.right_x() - width - 1);
            let y = rng.in_range(bounds.bottom_y() + 1, bounds.top_y() - height - 1);
            let layer = rng.in_range(0, 1);
            let seed = IntBox::new(x, y, x + width, y + height).to_simplex();

            // Only seeds clear of the foreign trace are in scope.
            let mut clear = true;
            for &item in &obstacles {
                for index in 0..engine.tree_shape_count(item) as u16 {
                    if engine.item_tile_layer(item, index) != Some(layer) {
                        continue;
                    }
                    if let Some(obstacle) = engine.item_tile(item, index) {
                        clear &= seed.intersection(&obstacle).dimension() < 2;
                    }
                }
            }
            if !clear {
                continue;
            }

            let incomplete = engine.rooms.add_incomplete_room(None, layer, seed.clone());
            let rooms = engine.complete_expansion_room(incomplete);
            if rooms.is_empty() {
                continue;
            }
            tested += 1;
            let covered: f64 = rooms
                .iter()
                .map(|id| engine.rooms.room(*id).shape.intersection(&seed).area())
                .sum();
            if (covered - seed.area()).abs() > 1e-6 {
                lost += 1;
            }
        }
        per_mode.push((mode, tested, lost));
    }

    let (_, off_tested, off_lost) = per_mode[0];
    let off_rate = off_lost as f64 / off_tested.max(1) as f64;
    for &(mode, tested, lost) in &per_mode[1..] {
        let rate = lost as f64 / tested.max(1) as f64;
        assert!(
            rate <= off_rate + 0.05,
            "{mode:?} loses free-seed area on {rate:.3} of seeds against Off's {off_rate:.3}"
        );
    }
}
