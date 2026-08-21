//! The three-valued side/orientation enum, ported from
//! `app.freerouting.geometry.planar.Side`.

/// An enum with the three values `OnTheLeft`, `OnTheRight` and `Collinear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    OnTheLeft,
    OnTheRight,
    Collinear,
}

impl Side {
    /// Returns `OnTheLeft` if `value > 0`, `OnTheRight` if `value < 0`, and
    /// `Collinear` if `value == 0`.
    pub fn of(value: f64) -> Side {
        if value > 0.0 {
            Side::OnTheLeft
        } else if value < 0.0 {
            Side::OnTheRight
        } else {
            Side::Collinear
        }
    }

    /// Returns the opposite side; `Collinear` is its own opposite.
    pub fn negate(self) -> Side {
        match self {
            Side::OnTheLeft => Side::OnTheRight,
            Side::OnTheRight => Side::OnTheLeft,
            Side::Collinear => Side::Collinear,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_sign_mapping() {
        assert_eq!(Side::of(1.0), Side::OnTheLeft);
        assert_eq!(Side::of(-1.0), Side::OnTheRight);
        assert_eq!(Side::of(0.0), Side::Collinear);
    }

    #[test]
    fn negate_is_involution() {
        assert_eq!(Side::OnTheLeft.negate(), Side::OnTheRight);
        assert_eq!(Side::OnTheRight.negate(), Side::OnTheLeft);
        assert_eq!(Side::Collinear.negate(), Side::Collinear);
    }
}
