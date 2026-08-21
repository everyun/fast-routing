//! Auxiliary functions with `BigInteger` parameters, ported from
//! `app.freerouting.datastructures.BigIntAux`.

use num_bigint::BigInt;

/// Auxiliary functions for arbitrary-precision integer arithmetic.
///
/// The original Java class is a collection of static helpers (a non-instantiable
/// utility). The Rust equivalent groups the same functions behind a unit struct
/// so call sites read `BigIntAux::binary_gcd(...)`.
pub struct BigIntAux;

impl BigIntAux {
    /// Calculates the determinant of the vectors `(x1, y1)` and `(x2, y2)`.
    pub fn determinant(x1: &BigInt, y1: &BigInt, x2: &BigInt, y2: &BigInt) -> BigInt {
        let tmp1 = x1 * y2;
        let tmp2 = x2 * y1;
        tmp1 - tmp2
    }

    /// Adds two rational coordinates `(x, y, z)` and `(x', y', z')`.
    ///
    /// The input and output convention is `(numerator_x, numerator_y, denominator)`.
    /// When the two denominators are equal the numerators are added directly;
    /// otherwise a common denominator equal to the product is used (matching the
    /// upstream implementation, which deliberately avoids least-common-multiple
    /// computation).
    pub fn add_rational_coordinates(
        first: (&BigInt, &BigInt, &BigInt),
        second: (&BigInt, &BigInt, &BigInt),
    ) -> (BigInt, BigInt, BigInt) {
        let (fx, fy, fz) = first;
        let (sx, sy, sz) = second;

        if fz == sz {
            // both rational numbers have the same denominator
            let denom = fz.clone();
            let x = fx + sx;
            let y = fy + sy;
            (x, y, denom)
        } else {
            // multiply both denominators for the new denominator to be on the
            // safe side (taking the least common multiple would be optimal).
            let denom = fz * sz;
            let x = fx * sz + sx * fz;
            let y = fy * sz + sy * fz;
            (x, y, denom)
        }
    }

    /// Calculates the greatest common divisor of `a` and `b`, interpreted as
    /// unsigned integers (i.e. the caller must pass non-negative values).
    ///
    /// This is a faithful port of the binary (Stein's) GCD algorithm copied by
    /// the upstream project from the JDK internals. It is only ever called with
    /// the absolute values of vector coordinates.
    pub fn binary_gcd(a: i32, b: i32) -> i32 {
        // The upstream implementation works on `int` but treats values as
        // unsigned; `u32` is the natural Rust equivalent. The inputs are always
        // non-negative at the call site (Math.abs of coordinates).
        let a = a as u32;
        let b = b as u32;

        if b == 0 {
            return a as i32;
        }
        if a == 0 {
            return b as i32;
        }

        let tz_a = a.trailing_zeros();
        let a = a >> tz_a;
        let tz_b = b.trailing_zeros();
        let b = b >> tz_b;
        let t = tz_a.min(tz_b);

        let (mut a, mut b) = (a, b);
        while a != b {
            if a > b {
                a -= b;
                a >>= a.trailing_zeros();
            } else {
                b -= a;
                b >>= b.trailing_zeros();
            }
        }

        (a << t) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_gcd_known_values() {
        assert_eq!(BigIntAux::binary_gcd(12, 8), 4);
        assert_eq!(BigIntAux::binary_gcd(17, 13), 1);
        assert_eq!(BigIntAux::binary_gcd(0, 7), 7);
        assert_eq!(BigIntAux::binary_gcd(7, 0), 7);
        assert_eq!(BigIntAux::binary_gcd(60, 48), 12);
        assert_eq!(BigIntAux::binary_gcd(1, 1), 1);
    }

    #[test]
    fn binary_gcd_reduces_direction_vectors() {
        // Normalizing the direction (6, 3) should yield (2, 1).
        let (dx, dy): (i32, i32) = (6, 3);
        let gcd = BigIntAux::binary_gcd(dx.abs(), dy.abs());
        assert_eq!((dx / gcd, dy / gcd), (2, 1));
    }

    #[test]
    fn determinant_value() {
        let det = BigIntAux::determinant(
            &BigInt::from(3),
            &BigInt::from(4),
            &BigInt::from(5),
            &BigInt::from(6),
        );
        // 3*6 - 5*4 = -2
        assert_eq!(det, BigInt::from(-2));
    }

    #[test]
    fn add_rational_coordinates_same_and_different_denominator() {
        let a = (BigInt::from(1), BigInt::from(2), BigInt::from(3));
        let b = (BigInt::from(4), BigInt::from(5), BigInt::from(3));
        let (x, y, z) = BigIntAux::add_rational_coordinates(
            (&a.0, &a.1, &a.2),
            (&b.0, &b.1, &b.2),
        );
        assert_eq!((x, y, z), (BigInt::from(5), BigInt::from(7), BigInt::from(3)));

        let c = (BigInt::from(1), BigInt::from(1), BigInt::from(2));
        let d = (BigInt::from(1), BigInt::from(1), BigInt::from(3));
        let (x, y, z) = BigIntAux::add_rational_coordinates(
            (&c.0, &c.1, &c.2),
            (&d.0, &d.1, &d.2),
        );
        assert_eq!(z, BigInt::from(6));
        assert_eq!(x, BigInt::from(1) * 3 + BigInt::from(1) * 2); // 5
        assert_eq!(y, BigInt::from(5));
    }
}
