//! The abstract vector enum, ported from `app.freerouting.geometry.planar.Vector`.
//!
//! Replicates the Java double-dispatch pattern between `IntVector` and
//! `RationalVector`.

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, Zero};

use crate::planar::{
    direction::Direction, float_point::FloatPoint, int_point::IntPoint, int_vector::IntVector,
    limits::Limits, point::Point, rational_point::RationalPoint, rational_vector::RationalVector,
    side::Side,
};
use fr_datastructures::Signum;

/// An abstract vector in the plane, either an integer vector (the fast, common
/// path) or an exact rational vector.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Vector {
    Int(IntVector),
    Rational(RationalVector),
}

impl Vector {
    /// The standard zero vector.
    pub const ZERO: Vector = Vector::Int(IntVector::ZERO);

    /// Creates a vector `(x, y)` in the plane. If either coordinate exceeds
    /// `CRIT_INT`, a rational vector is constructed.
    pub fn get_instance(x: i32, y: i32) -> Self {
        let result = IntVector::new(x, y);
        if x.abs() > Limits::CRIT_INT || y.abs() > Limits::CRIT_INT {
            Vector::Rational(RationalVector::from_int_vector(&result))
        } else {
            Vector::Int(result)
        }
    }

    /// Factory method for creating a `Vector` from 3 `BigInt`s (x / z, y / z).
    pub fn get_instance_bigint(mut x: BigInt, mut y: BigInt, mut z: BigInt) -> Self {
        if z.sign() == Sign::Minus {
            x = -x;
            y = -y;
            z = -z;
        }
        if (&x % &z).is_zero() && (&y % &z).is_zero() {
            x = &x / &z;
            y = &y / &z;
            z = BigInt::from(1);
        }
        if z == BigInt::from(1)
            && x.abs() <= Limits::crit_int_big()
            && y.abs() <= Limits::crit_int_big()
        {
            let xi = i32::try_from(&x).expect("x within CRIT_INT");
            let yi = i32::try_from(&y).expect("y within CRIT_INT");
            return Vector::Int(IntVector::new(xi, yi));
        }
        Vector::Rational(RationalVector::new(x, y, z))
    }

    /// Returns true if this vector is equal to the zero vector.
    pub fn is_zero(&self) -> bool {
        match self {
            Vector::Int(v) => v.is_zero(),
            Vector::Rational(v) => v.is_zero(),
        }
    }

    /// Returns the vector such that `this + this.negate()` is zero.
    pub fn negate(&self) -> Vector {
        match self {
            Vector::Int(v) => Vector::Int(IntVector::new(-v.x, -v.y)),
            Vector::Rational(v) => Vector::Rational(v.negate()),
        }
    }

    /// Adds `other` to this vector.
    pub fn add(&self, other: &Vector) -> Vector {
        match (self, other) {
            (Vector::Int(a), Vector::Int(b)) => Vector::Int(a.add_int(b)),
            (Vector::Int(a), Vector::Rational(b)) => {
                let ra = RationalVector::from_int_vector(a);
                Vector::Rational(ra.add_rational(b))
            }
            (Vector::Rational(a), Vector::Int(b)) => {
                let rb = RationalVector::from_int_vector(b);
                Vector::Rational(a.add_rational(&rb))
            }
            (Vector::Rational(a), Vector::Rational(b)) => Vector::Rational(a.add_rational(b)),
        }
    }

    /// Adds this vector to an `IntPoint`.
    pub fn add_to_int_point(&self, point: &IntPoint) -> Point {
        match self {
            Vector::Int(v) => Point::Int(IntPoint::new(point.x + v.x, point.y + v.y)),
            Vector::Rational(v) => {
                let iv = IntVector::new(point.x, point.y);
                Point::Rational(v.add_to_int_point(&iv))
            }
        }
    }

    /// Adds this vector to a `RationalPoint`.
    pub fn add_to_rational_point(&self, point: &RationalPoint) -> Point {
        match self {
            Vector::Int(v) => {
                let rv = RationalVector::from_int_vector(v);
                Point::Rational(rv.add_to_rational_point(point))
            }
            Vector::Rational(v) => Point::Rational(v.add_to_rational_point(point)),
        }
    }

    /// Let L be the line from the Zero Vector to `other`. The function returns
    /// `Side::OnTheLeft` if this vector is on the left of L, `Side::OnTheRight` if
    /// this vector is on the right of L, and `Side::Collinear` if collinear.
    pub fn side_of(&self, other: &Vector) -> Side {
        match (self, other) {
            (Vector::Int(a), Vector::Int(b)) => {
                // Java IntVector.sideOf(Vector other) -> other.sideOf(this).negate()
                // Java IntVector.sideOf(IntVector other) -> Side.of(other.x * y - other.y * x)
                // where (x, y) = this. So a.side_of(b) = Side.of(b.x * a.y - b.y * a.x).
                let det = (b.x as f64) * (a.y as f64) - (b.y as f64) * (a.x as f64);
                Side::of(det)
            }
            (Vector::Int(a), Vector::Rational(b)) => {
                let ra = RationalVector::from_int_vector(a);
                b.side_of_rational(&ra).negate()
            }
            (Vector::Rational(a), Vector::Int(b)) => {
                let rb = RationalVector::from_int_vector(b);
                a.side_of_rational(&rb)
            }
            (Vector::Rational(a), Vector::Rational(b)) => a.side_of_rational(b),
        }
    }

    /// Returns true if the vector is horizontal or vertical.
    pub fn is_orthogonal(&self) -> bool {
        match self {
            Vector::Int(v) => v.is_orthogonal(),
            Vector::Rational(v) => v.x.is_zero() || v.y.is_zero(),
        }
    }

    /// Returns true if the vector is diagonal (45° multiple).
    pub fn is_diagonal(&self) -> bool {
        match self {
            Vector::Int(v) => v.is_diagonal(),
            Vector::Rational(v) => v.x.abs() == v.y.abs(),
        }
    }

    /// Returns true if the vector is orthogonal or diagonal.
    pub fn is_multiple_of_45_degree(&self) -> bool {
        self.is_orthogonal() || self.is_diagonal()
    }

    /// Returns the sign of the scalar product with `other`.
    pub fn projection(&self, other: &Vector) -> Signum {
        match (self, other) {
            (Vector::Int(a), Vector::Int(b)) => a.projection_int(b),
            (Vector::Int(a), Vector::Rational(b)) => {
                let ra = RationalVector::from_int_vector(a);
                b.projection_rational(&ra)
            }
            (Vector::Rational(a), Vector::Int(b)) => {
                let rb = RationalVector::from_int_vector(b);
                a.projection_rational(&rb)
            }
            (Vector::Rational(a), Vector::Rational(b)) => a.projection_rational(b),
        }
    }

    /// Approximated scalar product with `other`.
    pub fn scalar_product(&self, other: &Vector) -> f64 {
        match (self, other) {
            (Vector::Int(a), Vector::Int(b)) => a.scalar_product_int(b),
            (Vector::Int(a), Vector::Rational(b)) => {
                let ra = RationalVector::from_int_vector(a);
                b.scalar_product_rational(&ra)
            }
            (Vector::Rational(a), Vector::Int(b)) => {
                let rb = RationalVector::from_int_vector(b);
                a.scalar_product_rational(&rb)
            }
            (Vector::Rational(a), Vector::Rational(b)) => a.scalar_product_rational(b),
        }
    }

    /// Approximates coordinates with float coordinates.
    pub fn to_float(&self) -> FloatPoint {
        match self {
            Vector::Int(v) => v.to_float(),
            Vector::Rational(v) => v.to_float(),
        }
    }

    /// Turns this vector by `factor` times 90 degrees.
    pub fn turn_90_degree(&self, factor: i32) -> Vector {
        match self {
            Vector::Int(v) => Vector::Int(v.turn_90_degree_int(factor)),
            Vector::Rational(v) => Vector::Rational(v.turn_90_degree(factor)),
        }
    }

    /// Mirrors this vector at the x axis.
    pub fn mirror_at_x_axis(&self) -> Vector {
        match self {
            Vector::Int(v) => Vector::Int(v.mirror_at_x_axis_int()),
            Vector::Rational(v) => Vector::Rational(v.mirror_at_x_axis()),
        }
    }

    /// Mirrors this vector at the y axis.
    pub fn mirror_at_y_axis(&self) -> Vector {
        match self {
            Vector::Int(v) => Vector::Int(v.mirror_at_y_axis_int()),
            Vector::Rational(v) => Vector::Rational(v.mirror_at_y_axis()),
        }
    }

    /// Returns an approximation of the Euclidean length of this vector.
    pub fn length_approx(&self) -> f64 {
        self.to_float().size()
    }

    /// Returns an approximation of the cosine of the angle with `other`.
    pub fn cos_angle(&self, other: &Vector) -> f64 {
        let mut result = self.scalar_product(other);
        result /= self.to_float().size() * other.to_float().size();
        result
    }

    /// Returns an approximation of the signed angle between this vector and `other`.
    pub fn angle_approx_to(&self, other: &Vector) -> f64 {
        let mut result = self.cos_angle(other).acos();
        if self.side_of(other) == Side::OnTheLeft {
            result = -result;
        }
        result
    }

    /// Returns an approximation of the signed angle between this vector and the x axis.
    pub fn angle_approx(&self) -> f64 {
        let other = Vector::Int(IntVector::new(1, 0));
        other.angle_approx_to(self)
    }

    /// Returns the normalized direction of this vector.
    pub fn to_normalized_direction(&self) -> Direction {
        match self {
            Vector::Int(v) => v.to_normalized_direction(),
            Vector::Rational(v) => v.to_normalized_direction(),
        }
    }
}
