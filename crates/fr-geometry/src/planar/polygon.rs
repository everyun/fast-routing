//! Simple polygon defined by a sequence of corner points.
//!
//! Ported from `app.freerouting.geometry.planar.Polygon`.

use std::cmp::Ordering;

use num_bigint::{BigInt, Sign};
use num_traits::Zero;

use crate::planar::{point::Point, side::Side};

/// A Polygon is a list of points in the plane where no 2 consecutive points
/// may be equal and no 3 consecutive points collinear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polygon {
    corners: Vec<Point>,
}

impl Polygon {
    /// Creates a polygon from a slice of points. Multiple consecutive points
    /// and collinear points with their neighbours are removed.
    pub fn new(point_arr: &[Point]) -> Self {
        if point_arr.is_empty() {
            return Polygon {
                corners: Vec::new(),
            };
        }
        let mut corners: Vec<Point> = point_arr.to_vec();

        let mut corner_removed = true;
        while corner_removed {
            corner_removed = false;
            if corners.is_empty() {
                break;
            }

            // Remove consecutive equal points
            let mut i = 0;
            while i + 1 < corners.len() {
                if corners[i] == corners[i + 1] {
                    corners.remove(i + 1);
                    corner_removed = true;
                } else {
                    i += 1;
                }
            }

            // Remove points collinear with previous and next
            if corners.len() >= 3 {
                let mut j = 1;
                while j + 1 < corners.len() {
                    let prev = &corners[j - 1];
                    let curr = &corners[j];
                    let next = &corners[j + 1];
                    if curr.side_of_points(prev, next) == Side::Collinear {
                        corners.remove(j);
                        corner_removed = true;
                        break;
                    }
                    j += 1;
                }
            }
        }

        Polygon { corners }
    }

    /// Returns the slice / array of corners of this polygon.
    pub fn corner_array(&self) -> &[Point] {
        &self.corners
    }

    /// Reverts the order of the corners of this polygon.
    pub fn revert_corners(&self) -> Polygon {
        let mut rev = self.corners.clone();
        rev.reverse();
        Polygon { corners: rev }
    }

    /// Twice the signed area of this polygon, closed by the edge from the last
    /// corner back to the first (the shoelace sum).
    ///
    /// Positive means counter-clockwise, negative clockwise and `0` a
    /// degenerate polygon (fewer than 3 corners, or all corners collinear).
    ///
    /// Integer coordinates are bounded by `Limits::CRIT_INT` (`2^25`), so every
    /// term `x_i * y_(i+1) - x_(i+1) * y_i` fits comfortably into `i64` and the
    /// sum of them into `i128` — no `f64` is involved, so the orientation is
    /// exact. Returns `None` if any corner is rational, because twice the area
    /// is then not an integer; use [`Polygon::is_clockwise`], which stays exact
    /// for those as well.
    pub fn corner_area(&self) -> Option<i128> {
        let corner_arr = &self.corners;
        if corner_arr.len() < 3 {
            return Some(0);
        }
        let mut sum: i128 = 0;
        for i in 0..corner_arr.len() {
            let curr = match &corner_arr[i] {
                Point::Int(ip) => *ip,
                Point::Rational(_) => return None,
            };
            let next = match &corner_arr[(i + 1) % corner_arr.len()] {
                Point::Int(ip) => *ip,
                Point::Rational(_) => return None,
            };
            let term = (curr.x as i64) * (next.y as i64) - (next.x as i64) * (curr.y as i64);
            sum += term as i128;
        }
        Some(sum)
    }

    /// Exact orientation of this polygon, closed by the edge from the last
    /// corner back to the first.
    ///
    /// `Some(true)` is clockwise, `Some(false)` counter-clockwise and `None` a
    /// degenerate polygon whose signed area is 0. Rational corners take an
    /// exact `BigInt` fallback, which never runs for the integer polygons the
    /// board reader produces.
    pub fn is_clockwise(&self) -> Option<bool> {
        let area_sign = match self.corner_area() {
            Some(area) => area.cmp(&0),
            None => self.rational_corner_area_sign(),
        };
        match area_sign {
            Ordering::Less => Some(true),
            Ordering::Greater => Some(false),
            Ordering::Equal => None,
        }
    }

    /// Sign of twice the signed area for a polygon with rational corners.
    ///
    /// Corner `k` is the projective triple `(x_k, y_k, z_k)` with `z_k >= 0`,
    /// so a shoelace term is `(x_i * y_j - x_j * y_i) / (z_i * z_j)`. The terms
    /// are added as exact rationals (common denominator = product of the two
    /// denominators, the same convention `BigIntAux::add_rational_coordinates`
    /// uses); the accumulated denominator stays positive, so the sign of the
    /// numerator is the sign of the area.
    fn rational_corner_area_sign(&self) -> Ordering {
        let corner_arr = &self.corners;
        if corner_arr.len() < 3 {
            return Ordering::Equal;
        }
        let homogeneous = |p: &Point| -> (BigInt, BigInt, BigInt) {
            match p {
                Point::Int(ip) => (BigInt::from(ip.x), BigInt::from(ip.y), BigInt::from(1)),
                Point::Rational(rp) => (rp.x.clone(), rp.y.clone(), rp.z.clone()),
            }
        };
        let mut num = BigInt::from(0);
        let mut den = BigInt::from(1);
        for i in 0..corner_arr.len() {
            let (xi, yi, zi) = homogeneous(&corner_arr[i]);
            let (xj, yj, zj) = homogeneous(&corner_arr[(i + 1) % corner_arr.len()]);
            let term_num = &xi * &yj - &xj * &yi;
            let term_den = &zi * &zj;
            // A corner at infinity (`z == 0`) has no affine position, so the
            // polygon has no well-defined area.
            if term_den.is_zero() {
                return Ordering::Equal;
            }
            num = &num * &term_den + &term_num * &den;
            den = &den * &term_den;
        }
        // `z >= 0` holds by construction, so `den > 0` and the numerator alone
        // carries the sign.
        match num.sign() {
            Sign::Minus => Ordering::Less,
            Sign::Plus => Ordering::Greater,
            Sign::NoSign => Ordering::Equal,
        }
    }

    /// Returns the winding number of this polygon, treated as closed.
    /// Positive (> 0) for counter-clockwise, negative (< 0) for clockwise.
    ///
    /// Divergence from upstream: Java sums approximated corner angles, a
    /// turning-number computation whose `f64` differences are not reduced into
    /// `(-pi, pi]` — the port of it returned `0` even for an axis-parallel
    /// square. The orientation is now taken from the exact signed area
    /// ([`Polygon::is_clockwise`]), so the result is `+1` / `-1` / `0` and
    /// agrees with the turning number for every simple polygon.
    pub fn winding_number_after_closing(&self) -> i32 {
        match self.is_clockwise() {
            Some(true) => -1,
            Some(false) => 1,
            None => 0,
        }
    }
}
