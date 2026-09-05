//! Brute-force verification of `IntOctagon::normalize` and the
//! octagon <-> simplex round trip.
//!
//! `normalize` must mutually clamp all 8 bounds to a fixed point without
//! changing the region it represents, rounding each bound *outward* (the
//! diagonal border lines meet at half-integer points). The oracle here scans
//! the doubled lattice, on which every octagon vertex is an integer point.

use fr_geometry::planar::{
    area::Area, int_octagon::IntOctagon, int_point::IntPoint, point::Point, simplex::Simplex,
};

/// Deterministic xorshift so the fuzz is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo + 1) as u64)) as i64
    }
}

/// Exact expected normalization, computed on the half-integer lattice.
///
/// Every vertex of an octagon bounded by lines x=c, y=c, x-y=c, x+y=c with
/// integer c is a half-integer point, so scanning the doubled lattice finds the
/// exact continuous extrema of x, y, x-y and x+y. The stored bounds must be
/// rounded *outward* (floor for lower bounds, ceil for upper bounds) so that the
/// represented region is unchanged.
fn brute_force_normalize(o: IntOctagon) -> Option<IntOctagon> {
    let (lx, ly, rx, uy) = (
        o.left_x as i64,
        o.bottom_y as i64,
        o.right_x as i64,
        o.top_y as i64,
    );
    let (ulx, lrx, llx, urx) = (
        o.upper_left_diagonal_x as i64,
        o.lower_right_diagonal_x as i64,
        o.lower_left_diagonal_x as i64,
        o.upper_right_diagonal_x as i64,
    );

    let mut min_u = i64::MAX;
    let mut max_u = i64::MIN;
    let mut min_v = i64::MAX;
    let mut max_v = i64::MIN;
    let mut min_d = i64::MAX; // u - v  (= 2*(x-y))
    let mut max_d = i64::MIN;
    let mut min_s = i64::MAX; // u + v  (= 2*(x+y))
    let mut max_s = i64::MIN;
    let mut any = false;

    for u in (2 * lx)..=(2 * rx) {
        for v in (2 * ly)..=(2 * uy) {
            if u - v < 2 * ulx || u - v > 2 * lrx || u + v < 2 * llx || u + v > 2 * urx {
                continue;
            }
            any = true;
            min_u = min_u.min(u);
            max_u = max_u.max(u);
            min_v = min_v.min(v);
            max_v = max_v.max(v);
            min_d = min_d.min(u - v);
            max_d = max_d.max(u - v);
            min_s = min_s.min(u + v);
            max_s = max_s.max(u + v);
        }
    }
    if !any {
        return None;
    }
    let fl = |v: i64| v.div_euclid(2);
    let ce = |v: i64| (v + 1).div_euclid(2);
    Some(IntOctagon::new(
        fl(min_u) as i32,
        fl(min_v) as i32,
        ce(max_u) as i32,
        ce(max_v) as i32,
        fl(min_d) as i32,
        ce(max_d) as i32,
        fl(min_s) as i32,
        ce(max_s) as i32,
    ))
}

#[test]
fn fuzz_normalize_matches_brute_force() {
    let mut rng = Rng(0x2026_0828_dead_beef);
    let mut checked_nonempty = 0usize;
    let mut checked_empty = 0usize;
    for _ in 0..20_000 {
        let lx = rng.range(-8, 8) as i32;
        let rx = lx + rng.range(0, 12) as i32;
        let ly = rng.range(-8, 8) as i32;
        let uy = ly + rng.range(0, 12) as i32;
        let ulx = rng.range(-16, 16) as i32;
        let lrx = ulx + rng.range(0, 24) as i32;
        let llx = rng.range(-16, 16) as i32;
        let urx = llx + rng.range(0, 24) as i32;
        let oct = IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx);

        let got = oct.normalize();
        match brute_force_normalize(oct) {
            None => {
                checked_empty += 1;
                assert!(
                    got.is_empty(),
                    "geometrically empty octagon {oct:?} normalized to non-empty {got:?}"
                );
            }
            Some(expected) => {
                checked_nonempty += 1;
                assert!(
                    !got.is_empty(),
                    "non-empty octagon {oct:?} normalized to empty"
                );
                assert_eq!(got, expected, "normalize mismatch for {oct:?}");
            }
        }
    }
    assert!(
        checked_nonempty > 500 && checked_empty > 100,
        "coverage: {checked_nonempty}/{checked_empty}"
    );
}

#[test]
fn fuzz_normalize_preserves_integer_point_set_and_is_idempotent() {
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for _ in 0..5_000 {
        let lx = rng.range(-8, 8) as i32;
        let rx = lx + rng.range(0, 12) as i32;
        let ly = rng.range(-8, 8) as i32;
        let uy = ly + rng.range(0, 12) as i32;
        let ulx = rng.range(-16, 16) as i32;
        let lrx = ulx + rng.range(0, 24) as i32;
        let llx = rng.range(-16, 16) as i32;
        let urx = llx + rng.range(0, 24) as i32;
        let oct = IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx);
        let n = oct.normalize();
        assert_eq!(n.normalize(), n, "normalize not idempotent for {oct:?}");
        for x in -20..=20 {
            for y in -20..=20 {
                let p = Point::Int(IntPoint::new(x, y));
                assert_eq!(
                    oct.contains_point(&p),
                    n.contains_point(&p),
                    "normalize changed integer membership of ({x},{y}) for {oct:?} -> {n:?}"
                );
            }
        }
    }
}

#[test]
fn fuzz_normalize_terminates_on_extreme_bounds() {
    // Values near the i32 limits and the CRIT_INT seeding used by
    // Simplex::to_int_octagon must not spin or overflow.
    let crit = fr_geometry::planar::limits::Limits::CRIT_INT;
    let cases = [
        IntOctagon::new(
            i32::MIN,
            i32::MIN,
            i32::MAX,
            i32::MAX,
            i32::MIN,
            i32::MAX,
            i32::MIN,
            i32::MAX,
        ),
        IntOctagon::new(-crit, -crit, crit, crit, -crit, crit, -crit, crit),
        IntOctagon::new(0, 0, i32::MAX, i32::MAX, -crit, crit, -crit, crit),
        IntOctagon::new(
            i32::MIN,
            0,
            i32::MAX,
            0,
            i32::MIN,
            i32::MAX,
            i32::MIN,
            i32::MAX,
        ),
        IntOctagon::new(
            i32::MIN + 1,
            i32::MIN + 1,
            i32::MAX - 1,
            i32::MAX - 1,
            -1,
            1,
            -1,
            1,
        ),
        IntOctagon::EMPTY,
    ];
    for c in cases {
        let n = c.normalize();
        assert_eq!(n.normalize(), n, "not idempotent for {c:?} -> {n:?}");
        for no in 0..8 {
            let _ = n.corner(no);
        }
    }
}

#[test]
fn fuzz_octagon_simplex_round_trip() {
    let mut rng = Rng(0xfeed_face_0bad_c0de);
    let mut two_d = 0usize;
    for _ in 0..2_000 {
        let lx = rng.range(-8, 8) as i32;
        let rx = lx + rng.range(0, 12) as i32;
        let ly = rng.range(-8, 8) as i32;
        let uy = ly + rng.range(0, 12) as i32;
        let ulx = rng.range(-16, 16) as i32;
        let lrx = ulx + rng.range(0, 24) as i32;
        let llx = rng.range(-16, 16) as i32;
        let urx = llx + rng.range(0, 24) as i32;
        let n = IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx).normalize();
        // Degenerate (point / segment) octagons must at least not panic.
        let s: Simplex = n.to_simplex();
        if n.is_empty() || n.dimension() < 2 {
            let _ = s.to_int_octagon();
            continue;
        }
        two_d += 1;
        let back = s.to_int_octagon();
        assert_eq!(back, Some(n), "round trip changed {n:?} (simplex {s:?})");
    }
    assert!(two_d > 200, "coverage {two_d}");
}
