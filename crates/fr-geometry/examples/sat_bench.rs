use fr_geometry::planar::{IntBox, IntOctagon, IntPoint, Line, Simplex};
use std::time::Instant;

fn main() {
    // Room-like octagon (8 lines) vs small box obstacle (4 lines) — the
    // carve loop's dominant pair.
    let room = IntOctagon::new(0, 0, 100000, 80000, -80000, 20000, 5000, 175000).to_simplex();
    let near_box = IntBox::new(90000, 70000, 110000, 90000).to_simplex(); // overlaps corner
    let far_box = IntBox::new(200000, 200000, 300000, 300000).to_simplex(); // disjoint
    let touch_box = IntBox::new(100000, 0, 120000, 50000).to_simplex(); // shares edge x=100000

    // also a free simplex room
    let tri = Simplex::get_instance(&[
        Line::new(IntPoint::new(0, 0), IntPoint::new(100000, 0)),
        Line::new(IntPoint::new(100000, 0), IntPoint::new(50000, 80000)),
        Line::new(IntPoint::new(50000, 80000), IntPoint::new(0, 0)),
    ]);

    const N: u32 = 200_000;
    for (name, a, b) in [
        ("oct vs near box", &room, &near_box),
        ("oct vs far box", &room, &far_box),
        ("oct vs touching box", &room, &touch_box),
        ("tri vs near box", &tri, &near_box),
    ] {
        let t0 = Instant::now();
        let mut acc = 0usize;
        for _ in 0..N {
            acc += (a.intersection(b).dimension() == 2) as usize;
        }
        let t_ref = t0.elapsed();
        let t0 = Instant::now();
        let mut acc2 = 0usize;
        for _ in 0..N {
            acc2 += a.has_2dim_overlap(b) as usize;
        }
        let t_new = t0.elapsed();
        assert_eq!(acc, acc2);
        println!(
            "{name}: exact {:7.1} ns  sat {:7.1} ns   (answer {})",
            t_ref.as_nanos() as f64 / N as f64,
            t_new.as_nanos() as f64 / N as f64,
            acc > 0
        );
    }
}
