//! Direction in the plane, ported from `app.freerouting.geometry.planar.Direction`
//! and `IntDirection`.
//!
//! A Direction is an equivalence class of vectors. Two vectors define the same
//! Direction if they point in the same direction. Directions are preferred over
//! angles because arithmetic is exact.

use crate::planar::{
    int_point::IntPoint, int_vector::IntVector, point::Point, side::Side, vector::Vector,
};
use fr_datastructures::{BigIntAux, Signum};

/// An exact direction represented as a normalized integer vector `(x, y)`
/// (i.e. `gcd(|x|, |y|) == 1` unless both are 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Direction {
    pub x: i32,
    pub y: i32,
}

impl Direction {
    /// Creates a direction from `(x, y)`, normalizing by the GCD.
    pub fn new(x: i32, y: i32) -> Self {
        if x == 0 && y == 0 {
            return Direction { x: 0, y: 0 };
        }
        let gcd = BigIntAux::binary_gcd(x.abs(), y.abs());
        if gcd > 1 {
            Direction {
                x: x / gcd,
                y: y / gcd,
            }
        } else {
            Direction { x, y }
        }
    }

    /// The null/zero direction.
    pub const NULL: Direction = Direction { x: 0, y: 0 };

    /// The direction to the east (+x).
    pub const RIGHT: Direction = Direction { x: 1, y: 0 };

    /// The direction to the northeast (+x, +y).
    pub const RIGHT45: Direction = Direction { x: 1, y: 1 };

    /// The direction to the north (+y).
    pub const UP: Direction = Direction { x: 0, y: 1 };

    /// The direction to the northwest (-x, +y).
    pub const UP45: Direction = Direction { x: -1, y: 1 };

    /// The direction to the west (-x).
    pub const LEFT: Direction = Direction { x: -1, y: 0 };

    /// The direction to the southwest (-x, -y).
    pub const LEFT45: Direction = Direction { x: -1, y: -1 };

    /// The direction to the south (-y).
    pub const DOWN: Direction = Direction { x: 0, y: -1 };

    /// The direction to the southeast (+x, -y).
    pub const DOWN45: Direction = Direction { x: 1, y: -1 };

    /// Creates a `Direction` from a `Vector`.
    pub fn from_vector(vector: &Vector) -> Self {
        vector.to_normalized_direction()
    }

    /// Calculates the direction from `from` to `to`. Returns `None` if `from == to`.
    pub fn from_int_points(from: &IntPoint, to: &IntPoint) -> Option<Self> {
        if from == to {
            None
        } else {
            let iv = IntVector::new(to.x - from.x, to.y - from.y);
            Some(iv.to_normalized_direction())
        }
    }

    /// Java compatibility alias `Direction.getInstance(p1, p2)`.
    pub fn get_instance(from: &Point, to: &Point) -> Self {
        to.difference_by(from).to_normalized_direction()
    }

    /// Java compatibility alias for integer points.
    pub fn get_instance_from_int_points(from: &IntPoint, to: &IntPoint) -> Self {
        IntVector::new(to.x - from.x, to.y - from.y).to_normalized_direction()
    }

    /// Creates a `Direction` whose angle with the x-axis is nearly equal to `angle`.
    pub fn from_angle_approx(angle: f64) -> Self {
        const SCALE_FACTOR: f64 = 10_000.0;
        let x = (angle.cos() * SCALE_FACTOR).round() as i32;
        let y = (angle.sin() * SCALE_FACTOR).round() as i32;
        IntVector::new(x, y).to_normalized_direction()
    }

    /// Returns a vector pointing into this direction.
    pub fn get_vector(&self) -> Vector {
        Vector::Int(IntVector::new(self.x, self.y))
    }

    /// Returns the integer vector for this direction.
    pub fn get_int_vector(&self) -> IntVector {
        IntVector::new(self.x, self.y)
    }

    /// Returns true if the direction is horizontal or vertical.
    pub fn is_orthogonal(&self) -> bool {
        self.x == 0 || self.y == 0
    }

    /// Returns true if the direction is diagonal.
    pub fn is_diagonal(&self) -> bool {
        self.x.abs() == self.y.abs()
    }

    /// Returns true if the direction is orthogonal or diagonal.
    pub fn is_multiple_of_45_degree(&self) -> bool {
        self.is_orthogonal() || self.is_diagonal()
    }

    /// Turns the direction by `factor` times 45 degrees.
    pub fn turn_45_degree(&self, factor: i32) -> Direction {
        let mut n = factor % 8;
        if n < 0 {
            n += 8;
        }
        let (new_x, new_y) = match n {
            0 => (self.x, self.y),
            1 => (self.x - self.y, self.x + self.y),
            2 => (-self.y, self.x),
            3 => (-self.x - self.y, self.x - self.y),
            4 => (-self.x, -self.y),
            5 => (self.y - self.x, -self.x - self.y),
            6 => (self.y, -self.x),
            7 => (self.x + self.y, self.y - self.x),
            _ => (0, 0),
        };
        Direction::new(new_x, new_y)
    }

    /// Returns the opposite direction.
    pub fn opposite(&self) -> Direction {
        Direction {
            x: -self.x,
            y: -self.y,
        }
    }

    /// Side of `other` direction.
    pub fn side_of(&self, other: &Direction) -> Side {
        self.get_vector().side_of(&other.get_vector())
    }

    /// Projection (scalar product sign) with `other`.
    pub fn projection(&self, other: &Direction) -> Signum {
        self.get_vector().projection(&other.get_vector())
    }

    /// Calculates an approximation of the direction in the middle of `this` and `other`.
    pub fn middle_approx(&self, other: &Direction) -> Direction {
        let v1 = self.get_vector().to_float();
        let v2 = other.get_vector().to_float();
        let l1 = v1.size();
        let l2 = v2.size();
        let x = v1.x / l1 + v2.x / l2;
        let y = v1.y / l1 + v2.y / l2;
        const SCALE_FACTOR: f64 = 1000.0;
        let vm = IntVector::new((x * SCALE_FACTOR).round() as i32, (y * SCALE_FACTOR).round() as i32);
        vm.to_normalized_direction()
    }

    /// Returns an approximation of the signed angle corresponding to this direction.
    pub fn angle_approx(&self) -> f64 {
        self.get_vector().angle_approx()
    }

    /// Faithful port of `IntDirection.compareTo(IntDirection other)`.
    ///
    /// Returns 1 if `self` has a strictly larger angle with the positive x-axis
    /// than `other`, 0 if equal, -1 otherwise.
    pub fn cmp_direction(&self, other: &Direction) -> i32 {
        if self.y > 0 {
            if other.y < 0 {
                return -1;
            }
            if other.y == 0 {
                if other.x > 0 {
                    return 1;
                }
                return -1;
            }
        } else if self.y < 0 {
            if other.y >= 0 {
                return 1;
            }
        } else {
            // self.y == 0
            if self.x > 0 {
                if other.y != 0 || other.x < 0 {
                    return -1;
                }
                return 0;
            }
            // self.x < 0
            if other.y > 0 || (other.y == 0 && other.x > 0) {
                return 1;
            }
            if other.y < 0 {
                return -1;
            }
            return 0;
        }

        // now both are in the same open horizontal half plane
        let determinant = (other.x as f64) * (self.y as f64) - (other.y as f64) * (self.x as f64);
        Signum::as_int(determinant)
    }

    /// Returns 1 if the angle between `p1` and this direction is bigger than
    /// between `p2` and this direction, 0 if equal, -1 otherwise.
    pub fn compare_from(&self, p1: &Direction, p2: &Direction) -> i32 {
        if p1.cmp_direction(self) >= 0 {
            if p2.cmp_direction(self) >= 0 {
                p1.cmp_direction(p2)
            } else {
                -1
            }
        } else if p2.cmp_direction(self) >= 0 {
            1
        } else {
            p1.cmp_direction(p2)
        }
    }
}

impl PartialOrd for Direction {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Direction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.cmp_direction(other) {
            1 => std::cmp::Ordering::Greater,
            -1 => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_45_degree_cycles_all_eight_octants() {
        let mut dir = Direction::RIGHT;
        let expected = [
            Direction::RIGHT45,
            Direction::UP,
            Direction::UP45,
            Direction::LEFT,
            Direction::LEFT45,
            Direction::DOWN,
            Direction::DOWN45,
            Direction::RIGHT,
        ];
        for exp in expected {
            dir = dir.turn_45_degree(1);
            assert_eq!(dir, exp);
        }
    }

    #[test]
    fn opposite_inverts() {
        assert_eq!(Direction::RIGHT.opposite(), Direction::LEFT);
        assert_eq!(Direction::UP.opposite(), Direction::DOWN);
        assert_eq!(Direction::RIGHT45.opposite(), Direction::LEFT45);
    }

    #[test]
    fn cmp_direction_angular_ordering() {
        // Counter-clockwise ordering starting from RIGHT (+x):
        // RIGHT < RIGHT45 < UP < UP45 < LEFT < LEFT45 < DOWN < DOWN45
        assert!(Direction::RIGHT < Direction::RIGHT45);
        assert!(Direction::RIGHT45 < Direction::UP);
        assert!(Direction::UP < Direction::LEFT);
        assert!(Direction::LEFT < Direction::DOWN);
    }
}
