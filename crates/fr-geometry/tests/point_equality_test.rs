//! Projective equality and `Hash` contract for `RationalPoint` / `Point`.
//!
//! `RationalPoint` stores unreduced homogeneous coordinates, so equality has to
//! cross-multiply and `Hash` has to hash the gcd-reduced triple. These tests
//! pin down that the two agree — including across the `Point::Int` /
//! `Point::Rational` boundary and for the degenerate triples that
//! `Line::intersection` can actually produce.

use num_bigint::BigInt;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use fr_geometry::planar::{
    int_point::IntPoint, line::Line, point::Point, rational_point::RationalPoint,
};

fn rp(x: i64, y: i64, z: i64) -> RationalPoint {
    RationalPoint::new(BigInt::from(x), BigInt::from(y), BigInt::from(z))
}

fn h<T: Hash>(v: &T) -> u64 {
    let mut s = DefaultHasher::new();
    v.hash(&mut s);
    s.finish()
}

/// Documents the degenerate triples reachable through `Line::intersection`,
/// which is why `PartialEq` has to special-case the all-zero triple.
#[test]
fn line_intersection_can_produce_degenerate_triples() {
    // A line intersected with itself: det == 0 and the numerators vanish too.
    let l = Line::new_from_coords(0, 0, 3, 7);
    match l.intersection(&l) {
        Point::Rational(r) => {
            assert_eq!(
                (r.x.to_string(), r.y.to_string(), r.z.to_string()),
                ("0".into(), "0".into(), "0".into())
            );
        }
        other => panic!("expected the all-zero rational triple, got {other:?}"),
    }

    // Parallel but distinct lines: a genuine point at infinity (z == 0).
    let a = Line::new_from_coords(0, 0, 3, 7);
    let b = Line::new_from_coords(1, 0, 4, 7);
    match a.intersection(&b) {
        Point::Rational(r) => {
            assert_eq!(r.z.to_string(), "0");
            assert!(!(r.x.to_string() == "0" && r.y.to_string() == "0"));
        }
        other => panic!("expected a point at infinity, got {other:?}"),
    }
}

/// `Line::intersection(l, l)` produces the all-zero triple, which is not a
/// projective point. Bare cross-multiplication would make it equal to every
/// other point, breaking transitivity and the `Eq`/`Hash` contract. It must
/// compare equal only to itself.
#[test]
fn zero_triple_is_equal_only_to_itself() {
    let zero = rp(0, 0, 0);
    let a = rp(1, 2, 3);
    let b = rp(5, 6, 7);
    assert_eq!(zero, rp(0, 0, 0));
    assert_ne!(zero, a, "(0,0,0) must not equal a finite point");
    assert_ne!(a, zero);
    assert_ne!(zero, b);
    assert_ne!(a, b);
    // Transitivity now holds.
    assert!(!(zero == a && zero == b && a != b));

    let pz = Point::Rational(zero.clone());
    let pi = Point::Int(IntPoint::new(0, 0));
    assert_ne!(pi, pz, "Point::Int(0,0) must not equal the all-zero triple");
    assert_ne!(pz, pi);

    // Eq/Hash contract holds under HashSet.
    let mut set: HashSet<Point> = HashSet::new();
    set.insert(pi.clone());
    assert!(!set.contains(&pz));
    set.insert(pz.clone());
    assert_eq!(set.len(), 2);
    assert!(set.contains(&pi) && set.contains(&pz));

    // A point at infinity is likewise never a finite point.
    assert_ne!(Point::Rational(rp(1, 0, 0)), pi);
    assert_ne!(pi, Point::Rational(rp(1, 0, 0)));
}

/// Eq must be an equivalence relation across a mixed population of triples.
#[test]
fn eq_is_reflexive_symmetric_and_transitive_on_a_mixed_population() {
    let pts: Vec<RationalPoint> = vec![
        rp(0, 0, 0),
        rp(1, 0, 0),
        rp(0, 1, 0),
        rp(-3, 5, 0),
        rp(0, 0, 1),
        rp(0, 0, 9),
        rp(1, 2, 3),
        rp(2, 4, 6),
        rp(-1, -2, 3),
        rp(5, 6, 7),
        rp(10, 12, 14),
        rp(3, -7, 1),
        rp(300, -700, 100),
    ];
    for a in &pts {
        assert_eq!(a, a, "not reflexive: {a:?}");
        for b in &pts {
            assert_eq!(a == b, b == a, "not symmetric: {a:?} {b:?}");
            if a == b {
                assert_eq!(h(a), h(b), "equal but different hash: {a:?} {b:?}");
            }
            for c in &pts {
                if a == b && b == c {
                    assert_eq!(a, c, "not transitive: {a:?} {b:?} {c:?}");
                }
            }
        }
    }
    // Same check lifted to Point, including the Int variant.
    let mixed: Vec<Point> = pts
        .iter()
        .cloned()
        .map(Point::Rational)
        .chain([
            Point::Int(IntPoint::new(0, 0)),
            Point::Int(IntPoint::new(3, -7)),
            Point::Int(IntPoint::new(5, 6)),
        ])
        .collect();
    for a in &mixed {
        assert_eq!(a, a);
        for b in &mixed {
            assert_eq!(a == b, b == a, "not symmetric: {a:?} {b:?}");
            if a == b {
                assert_eq!(h(a), h(b), "equal but different hash: {a:?} {b:?}");
            }
            for c in &mixed {
                if a == b && b == c {
                    assert_eq!(a, c, "not transitive: {a:?} {b:?} {c:?}");
                }
            }
        }
    }
}

#[test]
fn negative_z_is_rejected_by_the_constructor() {
    let r = std::panic::catch_unwind(|| rp(1, 2, -3));
    assert!(r.is_err(), "RationalPoint::new must reject z < 0");
}

#[test]
fn big_unreduced_triples_compare_and_hash_consistently() {
    let base = rp(3, -7, 1);
    for k in [2i64, 3, 97, 1_000_003, i32::MAX as i64] {
        let scaled = rp(3 * k, -7 * k, k);
        assert_eq!(base, scaled, "k={k}");
        assert_eq!(scaled, base, "k={k}");
        assert_eq!(h(&base), h(&scaled), "hash mismatch k={k}");
        assert_eq!(
            h(&Point::Int(IntPoint::new(3, -7))),
            h(&Point::Rational(scaled.clone())),
            "cross-variant hash mismatch k={k}"
        );
        assert_eq!(Point::Int(IntPoint::new(3, -7)), Point::Rational(scaled));
    }
}

/// Points at infinity with different directions all compare equal under
/// cross-multiplication (faithful to upstream Java), and hash identically.
#[test]
fn infinity_points_are_mutually_equal_and_hash_alike() {
    let a = rp(1, 0, 0);
    let b = rp(0, 1, 0);
    assert_eq!(a, b, "all non-degenerate z==0 triples compare equal");
    assert_eq!(h(&a), h(&b));
    assert_ne!(a, rp(1, 0, 1));
    assert!(!a.eq_int_point(&IntPoint::new(1, 0)));
    assert!(!rp(0, 0, 0).eq_int_point(&IntPoint::new(0, 0)));
    assert!(rp(0, 0, 5).eq_int_point(&IntPoint::new(0, 0)));
}

/// Eq must stay reflexive/symmetric on non-degenerate triples, and the
/// cross-variant Int/Rational comparison must be exact for huge values.
#[test]
fn cross_variant_equality_is_exact_for_extreme_int_points() {
    let ip = IntPoint::new(i32::MIN, i32::MAX);
    let exact = Point::Rational(RationalPoint::new(
        BigInt::from(i32::MIN) * 6,
        BigInt::from(i32::MAX) * 6,
        BigInt::from(6),
    ));
    let off_by_one = Point::Rational(RationalPoint::new(
        BigInt::from(i32::MIN) * 6 + BigInt::from(1),
        BigInt::from(i32::MAX) * 6,
        BigInt::from(6),
    ));
    assert_eq!(Point::Int(ip), exact);
    assert_eq!(exact, Point::Int(ip));
    assert_ne!(Point::Int(ip), off_by_one);
    assert_ne!(off_by_one, Point::Int(ip));
    assert_eq!(h(&Point::Int(ip)), h(&exact));
}
