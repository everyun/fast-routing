//! Rational point, ported from `app.freerouting.geometry.planar.RationalPoint`.
//!
//! Represents points in the projective plane via 3 coordinates `(x, y, z)` where
//! `z >= 0`. The affine point with rational coordinates is `(x / z, y / z)`.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, ToPrimitive, Zero};

use crate::planar::{
    float_point::FloatPoint, int_box::IntBox, int_octagon::IntOctagon, int_point::IntPoint,
    limits::Limits, line::Line, point::Point, rational_vector::RationalVector, side::Side,
    vector::Vector,
};
use fr_datastructures::BigIntAux;

/// A point in the projective plane with integer coordinates `(x, y, z)` and `z >= 0`.
#[derive(Debug, Clone, Eq)]
pub struct RationalPoint {
    pub x: BigInt,
    pub y: BigInt,
    pub z: BigInt,
}

impl PartialEq for RationalPoint {
    /// Exact projective equality by cross-multiplication, ported from Java's
    /// `RationalPoint.equals`: `(x1, y1, z1)` and `(x2, y2, z2)` represent the
    /// same point iff `x1 * z2 == x2 * z1` and `y1 * z2 == y2 * z1`, so
    /// unreduced triples of the same geometric point compare equal. The
    /// comparison is invariant under negating a whole triple, so it stays
    /// correct even for a (non-invariant) triple with `z < 0`.
    ///
    /// The all-zero triple `(0, 0, 0)` is not a projective point but *is*
    /// producible (`Line::intersection` of a line with itself returns it).
    /// Bare cross-multiplication would make it equal to every other point,
    /// which breaks transitivity and the `Eq`/`Hash` contract, so it is
    /// treated as equal only to itself.
    fn eq(&self, other: &Self) -> bool {
        let self_degenerate = self.x.is_zero() && self.y.is_zero() && self.z.is_zero();
        let other_degenerate = other.x.is_zero() && other.y.is_zero() && other.z.is_zero();
        if self_degenerate || other_degenerate {
            return self_degenerate && other_degenerate;
        }
        &self.x * &other.z == &other.x * &self.z && &self.y * &other.z == &other.y * &self.z
    }
}

impl Hash for RationalPoint {
    /// Hashes the canonical (gcd-reduced, sign-normalized) triple so that
    /// points comparing equal via cross-multiplication hash identically.
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (x, y, z) = self.reduced_coordinates();
        x.hash(state);
        y.hash(state);
        z.hash(state);
    }
}

impl RationalPoint {
    /// Creates a `RationalPoint` from 3 `BigInt`s. Panics if `z < 0`.
    pub fn new(x: BigInt, y: BigInt, z: BigInt) -> Self {
        assert!(z.sign() != Sign::Minus, "RationalPoint: z must be >= 0");
        RationalPoint { x, y, z }
    }

    /// Creates a `RationalPoint` from an `IntPoint`.
    pub fn from_int_point(point: &IntPoint) -> Self {
        RationalPoint {
            x: BigInt::from(point.x),
            y: BigInt::from(point.y),
            z: BigInt::from(1),
        }
    }

    /// Canonical representative of the projective triple: divided by
    /// `gcd(x, y, z)` and sign-normalized so that `z >= 0`.
    ///
    /// All points at infinity (`z == 0`) compare equal under
    /// cross-multiplication, so they share the single canonical form
    /// `(0, 0, 0)`. The degenerate all-zero triple (equal only to itself)
    /// shares that form too, which is a permitted hash collision.
    pub(crate) fn reduced_coordinates(&self) -> (BigInt, BigInt, BigInt) {
        use num_integer::Integer;
        if self.z.is_zero() {
            return (BigInt::from(0), BigInt::from(0), BigInt::from(0));
        }
        let gcd = self.x.gcd(&self.y).gcd(&self.z);
        let mut x = &self.x / &gcd;
        let mut y = &self.y / &gcd;
        let mut z = &self.z / &gcd;
        if z.sign() == Sign::Minus {
            x = -x;
            y = -y;
            z = -z;
        }
        (x, y, z)
    }

    /// Exact equality with an `IntPoint`: `(x, y, z)` equals `(a, b)` iff
    /// `x == a * z` and `y == b * z` (invariant under negating the triple).
    ///
    /// A triple with `z == 0` is a point at infinity (or the degenerate
    /// all-zero triple) and never equals a finite integer point.
    pub fn eq_int_point(&self, other: &IntPoint) -> bool {
        if self.z.is_zero() {
            return false;
        }
        self.x == BigInt::from(other.x) * &self.z && self.y == BigInt::from(other.y) * &self.z
    }

    /// Approximates coordinates by float coordinates.
    pub fn to_float(&self) -> FloatPoint {
        let zd = self.z.to_f64().unwrap_or(f64::MAX);
        if zd == 0.0 {
            FloatPoint::new(f32::MAX as f64, f32::MAX as f64)
        } else {
            let xd = self.x.to_f64().unwrap_or(f64::MAX);
            let yd = self.y.to_f64().unwrap_or(f64::MAX);
            FloatPoint::new(xd / zd, yd / zd)
        }
    }

    /// Stable identifier for deterministic tie-breaking.
    pub fn get_id_no(&self) -> i32 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish() as i32
    }

    /// Returns true if this is a point at infinity (`z == 0`).
    pub fn is_infinite(&self) -> bool {
        self.z.is_zero()
    }

    /// Smallest `IntBox` containing this point.
    ///
    /// The bounds are exact integer divisions of the projective triple, not
    /// `floor`/`ceil` of an `f64` quotient: a point sitting within one ulp
    /// below an integer converts to exactly that integer, and the "surrounding"
    /// box computed from the float would then be the degenerate box `[n, n]`,
    /// which does *not* contain the point. Containment is a topological
    /// decision, so it may not be decided by a rounded quotient.
    pub fn surrounding_box(&self) -> IntBox {
        if self.z.is_zero() {
            // Point at infinity: keep the upstream float behaviour.
            let fp = self.to_float();
            return IntBox::new(
                fp.x.floor() as i32,
                fp.y.floor() as i32,
                fp.x.ceil() as i32,
                fp.y.ceil() as i32,
            );
        }
        let (lx, rx) = self.floor_ceil(&self.x);
        let (ly, uy) = self.floor_ceil(&self.y);
        IntBox::new(lx, ly, rx, uy)
    }

    /// Smallest `IntOctagon` containing this point, with exact bounds; see
    /// [`RationalPoint::surrounding_box`].
    pub fn surrounding_octagon(&self) -> IntOctagon {
        if self.z.is_zero() {
            let fp = self.to_float();
            let lx = fp.x.floor() as i32;
            let ly = fp.y.floor() as i32;
            let rx = fp.x.ceil() as i32;
            let uy = fp.y.ceil() as i32;

            let tmp = fp.x - fp.y;
            let ulx = tmp.floor() as i32;
            let lrx = tmp.ceil() as i32;

            let tmp = fp.x + fp.y;
            let llx = tmp.floor() as i32;
            let urx = tmp.ceil() as i32;
            return IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx);
        }
        let (lx, rx) = self.floor_ceil(&self.x);
        let (ly, uy) = self.floor_ceil(&self.y);
        let (ulx, lrx) = self.floor_ceil(&(&self.x - &self.y));
        let (llx, urx) = self.floor_ceil(&(&self.x + &self.y));
        IntOctagon::new(lx, ly, rx, uy, ulx, lrx, llx, urx)
    }

    /// `(floor(numerator / z), ceil(numerator / z))` narrowed to `i32`,
    /// saturating like the `f64 as i32` cast it replaces. `z > 0` is required
    /// (the caller handles points at infinity), so no sign correction is needed.
    fn floor_ceil(&self, numerator: &BigInt) -> (i32, i32) {
        use num_integer::Integer;
        debug_assert!(!self.z.is_zero(), "floor_ceil needs a finite point");
        (
            Self::saturating_i32(&numerator.div_floor(&self.z)),
            Self::saturating_i32(&numerator.div_ceil(&self.z)),
        )
    }

    /// Narrows an exact bound to `i32`, saturating instead of wrapping.
    fn saturating_i32(value: &BigInt) -> i32 {
        value.to_i32().unwrap_or(if value.sign() == Sign::Minus {
            i32::MIN
        } else {
            i32::MAX
        })
    }

    /// Returns true if this point is in the interior or on the border of `box_`.
    pub fn is_contained_in(&self, box_: &IntBox) -> bool {
        let tmp = BigInt::from(box_.ll.x) * &self.z;
        if self.x < tmp {
            return false;
        }
        let tmp = BigInt::from(box_.ll.y) * &self.z;
        if self.y < tmp {
            return false;
        }
        let tmp = BigInt::from(box_.ur.x) * &self.z;
        if self.x > tmp {
            return false;
        }
        let tmp = BigInt::from(box_.ur.y) * &self.z;
        self.y <= tmp
    }

    /// Translates this point by `vector`.
    pub fn translate_by(&self, vector: &Vector) -> Point {
        if vector.is_zero() {
            Point::Rational(self.clone())
        } else {
            vector.add_to_rational_point(self)
        }
    }

    /// Difference vector `self - other`.
    pub fn difference_by_rational(&self, other: &RationalPoint) -> RationalVector {
        let (x, y, z) = BigIntAux::add_rational_coordinates(
            (&self.x, &self.y, &self.z),
            (&(-other.x.clone()), &(-other.y.clone()), &other.z),
        );
        RationalVector::new(x, y, z)
    }

    /// Side of line through `p1` and `p2`.
    pub fn side_of_points(&self, p1: &Point, p2: &Point) -> Side {
        let v1 = match p1 {
            Point::Int(ip) => {
                let rp = RationalPoint::from_int_point(ip);
                Vector::Rational(self.difference_by_rational(&rp))
            }
            Point::Rational(rp) => Vector::Rational(self.difference_by_rational(rp)),
        };
        let v2 = p2.difference_by(p1);
        v1.side_of(&v2)
    }

    /// Side of `line`.
    pub fn side_of(&self, line: &Line) -> Side {
        self.side_of_points(&Point::Int(line.a_int()), &Point::Int(line.b_int()))
    }

    /// Perpendicular projection onto `line`.
    pub fn perpendicular_projection(&self, line: &Line) -> Point {
        let v = line.b_int().difference_by_int(&line.a_int());
        let vxvx = BigInt::from((v.x as i64) * (v.x as i64));
        let vyvy = BigInt::from((v.y as i64) * (v.y as i64));
        let vxvy = BigInt::from((v.x as i64) * (v.y as i64));
        let mut denominator = &vxvx + &vyvy;
        let det = BigInt::from(line.a_int().determinant(&line.b_int()));

        let tmp1 = &vxvx * &self.x;
        let tmp2 = &vxvy * &self.y;
        let tmp1 = tmp1 + tmp2;
        let tmp2 = &det * BigInt::from(v.y) * &self.z;
        let mut proj_x = tmp1 + tmp2;

        let tmp1 = &vxvy * &self.x;
        let tmp2 = &vyvy * &self.y;
        let tmp1 = tmp1 + tmp2;
        let tmp2 = &det * BigInt::from(v.x) * &self.z;
        let mut proj_y = tmp1 - tmp2;

        if !denominator.is_zero() {
            if denominator.sign() == Sign::Minus {
                denominator = -denominator;
                proj_x = -proj_x;
                proj_y = -proj_y;
            }
            if (&proj_x % &denominator).is_zero() && (&proj_y % &denominator).is_zero() {
                proj_x = &proj_x / &denominator;
                proj_y = &proj_y / &denominator;
                if proj_x.abs() <= Limits::crit_int_big() && proj_y.abs() <= Limits::crit_int_big()
                {
                    let xi = i32::try_from(&proj_x).expect("proj_x fits i32");
                    let yi = i32::try_from(&proj_y).expect("proj_y fits i32");
                    return Point::Int(IntPoint::new(xi, yi));
                }
                denominator = BigInt::from(1);
            }
        }
        Point::Rational(RationalPoint::new(proj_x, proj_y, denominator))
    }

    /// Compares x coordinate with `other` rational point.
    ///
    /// Exact cross-multiplication `self.x * other.z` vs `other.x * self.z`;
    /// both denominators are non-negative by construction, so the order is
    /// preserved. See [`RationalPoint::cmp_products`] for the arithmetic tier.
    pub fn compare_x_rational(&self, other: &RationalPoint) -> i32 {
        match Self::cmp_products(&self.x, &other.z, &other.x, &self.z) {
            Some(ordering) => ordering as i32,
            None => (&self.x * &other.z).cmp(&(&other.x * &self.z)) as i32,
        }
    }

    /// Compares x coordinate with `other` int point.
    ///
    /// Exact: `self.x` against `self.z * other.x`, never `f64`.
    pub fn compare_x_int(&self, other: &IntPoint) -> i32 {
        match Self::cmp_int_product(&self.x, &self.z, other.x) {
            Some(ordering) => ordering as i32,
            None => self.x.cmp(&(&self.z * other.x)) as i32,
        }
    }

    /// Compares y coordinate with `other` rational point.
    pub fn compare_y_rational(&self, other: &RationalPoint) -> i32 {
        match Self::cmp_products(&self.y, &other.z, &other.y, &self.z) {
            Some(ordering) => ordering as i32,
            None => (&self.y * &other.z).cmp(&(&other.y * &self.z)) as i32,
        }
    }

    /// Compares y coordinate with `other` int point.
    pub fn compare_y_int(&self, other: &IntPoint) -> i32 {
        match Self::cmp_int_product(&self.y, &self.z, other.y) {
            Some(ordering) => ordering as i32,
            None => self.y.cmp(&(&self.z * other.y)) as i32,
        }
    }

    /// `a * b` against `c * d` on an `i128` fast path. Returns `None` when an
    /// operand or a product leaves `i128`, so that the caller falls back to the
    /// (exact, but allocating) `BigInt` path. Rational coordinates produced by
    /// line intersections inside the envelope stay far below 2^127, so the
    /// bignum path is cold.
    #[inline]
    fn cmp_products(a: &BigInt, b: &BigInt, c: &BigInt, d: &BigInt) -> Option<Ordering> {
        let left = a.to_i128()?.checked_mul(b.to_i128()?)?;
        let right = c.to_i128()?.checked_mul(d.to_i128()?)?;
        Some(left.cmp(&right))
    }

    /// `numerator` against `denominator * coordinate` on an `i128` fast path;
    /// `None` falls back to `BigInt` as in [`RationalPoint::cmp_products`].
    #[inline]
    fn cmp_int_product(
        numerator: &BigInt,
        denominator: &BigInt,
        coordinate: i32,
    ) -> Option<Ordering> {
        let right = denominator.to_i128()?.checked_mul(coordinate as i128)?;
        Some(numerator.to_i128()?.cmp(&right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(x: i64, y: i64, z: i64) -> RationalPoint {
        RationalPoint::new(BigInt::from(x), BigInt::from(y), BigInt::from(z))
    }

    fn hash_of(point: &RationalPoint) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        point.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn unreduced_triples_of_same_point_compare_equal() {
        // (1/3, 2/3) represented two different ways.
        assert_eq!(rp(1, 2, 3), rp(2, 4, 6));
        assert_eq!(rp(2, 4, 6), rp(1, 2, 3));
        // Same numerators, different denominator: different points.
        assert_ne!(rp(1, 2, 3), rp(1, 2, 4));
        assert_ne!(rp(1, 2, 3), rp(2, 2, 3));
    }

    #[test]
    fn hash_matches_for_unreduced_equal_points() {
        assert_eq!(hash_of(&rp(1, 2, 3)), hash_of(&rp(2, 4, 6)));
        assert_eq!(hash_of(&rp(-5, 7, 2)), hash_of(&rp(-15, 21, 6)));
        assert_eq!(rp(1, 2, 3).get_id_no(), rp(2, 4, 6).get_id_no());
    }

    #[test]
    fn eq_int_point_cross_multiplies() {
        let ip = IntPoint::new(3, -7);
        assert!(rp(3, -7, 1).eq_int_point(&ip));
        assert!(rp(6, -14, 2).eq_int_point(&ip));
        assert!(!rp(6, -13, 2).eq_int_point(&ip));
        assert!(!rp(6, -14, 3).eq_int_point(&ip));
    }
}
