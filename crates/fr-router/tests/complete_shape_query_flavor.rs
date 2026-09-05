//! The query-flavor invariant `complete_shape`'s obstacle gather depends on.
//!
//! `complete_shape` hands the search tree its running bound as a
//! `TileShape::Box`, not as `bounding.to_simplex()`. That is a pure performance
//! change — on Issue034 it took the router from 28.3 s to 21.5 s (`-mp 1`,
//! `/usr/bin/sample` attributed 52% of the run to the simplex arm of
//! `TileShape::intersects`) — and it is only sound because of two facts:
//!
//! 1. the box query returns a **superset** of the simplex query's obstacles
//!    (`IntBox::intersects` is inclusive, and any non-empty shape overlap
//!    implies a bounds overlap), and
//! 2. every extra obstacle is a **no-op** in the carve loop, so the completed
//!    tiling is identical.
//!
//! Both are asserted here. If a later change to `TileShape::intersects` or to
//! the tree's leaf filter breaks either one, the optimization silently starts
//! changing the room tiling — which is exactly the class of drift that stays
//! invisible until a gate run.
//!
//! The wall-clock half of the story is `timing_probe`, which is `#[ignore]`d:
//! it prints the two flavors' cost rather than asserting a ratio, because a
//! ratio assertion on a shared machine is a flaky test, not a measurement.

use fr_board::shape::TileShape;
use fr_geometry::planar::Simplex;
use fr_router::engine::Obstacle;
use fr_router::test_support::TestFixture;

/// The bound `complete_shape` queries with, and a contained shape inside it.
fn probe_shapes() -> (fr_geometry::planar::IntBox, Simplex) {
    let bound = fr_geometry::planar::IntBox::new(-6000, -6000, 6000, 6000);
    // Free space in every fixture: clear of both pads (±2000, 0) and of
    // `with_foreign_trace`'s vertical trace at x = 0.
    let contained = fr_geometry::planar::IntBox::new(990, 690, 1010, 710).to_simplex();
    (bound, contained)
}

fn gather(engine: &mut fr_router::engine::AutorouteEngine<'_>, query: &TileShape) -> Vec<Obstacle> {
    let mut obstacles = Vec::new();
    let mut own_net = Vec::new();
    engine.overlapping_obstacles_tile(query, Some(0), 0, &mut obstacles, &mut own_net);
    obstacles
}

#[test]
fn the_box_query_returns_a_superset_of_the_simplex_query() {
    for fixture in [
        TestFixture::two_layer_with_pads(),
        TestFixture::with_foreign_trace(),
        TestFixture::pure_smd_two_layer(),
    ] {
        let mut engine = fixture.engine();
        let (bound, _) = probe_shapes();

        let via_simplex = gather(&mut engine, &TileShape::Simplex(bound.to_simplex()));
        let via_box = gather(&mut engine, &TileShape::Box(bound));

        for obstacle in &via_simplex {
            assert!(
                via_box.contains(obstacle),
                "the box query dropped {obstacle:?} that the simplex query found"
            );
        }
        // Both lists are produced by the same sort, so the shared prefix order
        // must agree too — the carve loop is order-sensitive.
        let filtered: Vec<Obstacle> = via_box
            .iter()
            .copied()
            .filter(|o| via_simplex.contains(o))
            .collect();
        assert_eq!(
            filtered, via_simplex,
            "the box query reordered the obstacles the simplex query found"
        );
    }
}

#[test]
fn the_completed_tiling_is_the_same_under_both_query_flavors() {
    // `complete_shape` itself always uses the box query now, so the check that
    // matters is that the *obstacle set difference* cannot change its answer:
    // every obstacle the box query adds must miss the completed shape.
    for fixture in [
        TestFixture::two_layer_with_pads(),
        TestFixture::with_foreign_trace(),
    ] {
        let mut engine = fixture.engine();
        let (bound, contained) = probe_shapes();

        let via_simplex = gather(&mut engine, &TileShape::Simplex(bound.to_simplex()));
        let via_box = gather(&mut engine, &TileShape::Box(bound));
        let extras: Vec<Obstacle> = via_box
            .iter()
            .copied()
            .filter(|o| !via_simplex.contains(o))
            .collect();

        let completed = engine.complete_shape(None, &contained, 0, None, None);
        assert!(
            !completed.is_empty(),
            "a contained shape on an empty layer must complete to at least one room"
        );

        for extra in extras {
            let Some(shape) = engine.obstacle_shape(extra) else {
                continue;
            };
            for room in &completed {
                let overlap = room.shape.intersection(&shape);
                assert!(
                    overlap.dimension() < 2,
                    "{extra:?} is only in the box query's answer, yet it overlaps a \
                     completed room in 2 dimensions — the box query is no longer a \
                     behaviour-preserving substitution"
                );
            }
        }
    }
}

/// A coarse stage timer for the two query flavors. Run with
/// `cargo test -p fr-router --release --test complete_shape_query_flavor -- \
///  --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not an assertion: prints timings, never fails on them"]
fn timing_probe() {
    use std::time::Instant;

    let fixture = TestFixture::with_foreign_trace();
    let mut engine = fixture.engine();
    let (bound, _) = probe_shapes();
    const ROUNDS: u32 = 20_000;

    // Warm the tree and the scratch buffers.
    gather(&mut engine, &TileShape::Box(bound));

    let simplex_query = TileShape::Simplex(bound.to_simplex());
    let start = Instant::now();
    for _ in 0..ROUNDS {
        std::hint::black_box(gather(&mut engine, &simplex_query));
    }
    let simplex_elapsed = start.elapsed();

    let box_query = TileShape::Box(bound);
    let start = Instant::now();
    for _ in 0..ROUNDS {
        std::hint::black_box(gather(&mut engine, &box_query));
    }
    let box_elapsed = start.elapsed();

    println!(
        "overlapping_obstacles_tile over {ROUNDS} rounds:\n  \
         simplex query : {simplex_elapsed:?}\n  \
         box query     : {box_elapsed:?}\n  \
         speedup       : {:.2}x",
        simplex_elapsed.as_secs_f64() / box_elapsed.as_secs_f64().max(f64::MIN_POSITIVE)
    );
}
