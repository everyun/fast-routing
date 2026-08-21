//! Forty-five degree direction enum, ported from
//! `app.freerouting.geometry.planar.FortyfiveDegreeDirection`.

use crate::planar::direction::Direction;

/// Enum for the eight 45-degree directions, starting from `Right` (+x) in
/// counter-clockwise order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FortyfiveDegreeDirection {
    Right,
    Right45,
    Up,
    Up45,
    Left,
    Left45,
    Down,
    Down45,
}

impl FortyfiveDegreeDirection {
    /// Returns the exact `Direction` value for this 45-degree direction.
    pub fn get_direction(&self) -> Direction {
        match self {
            FortyfiveDegreeDirection::Right => Direction::RIGHT,
            FortyfiveDegreeDirection::Right45 => Direction::RIGHT45,
            FortyfiveDegreeDirection::Up => Direction::UP,
            FortyfiveDegreeDirection::Up45 => Direction::UP45,
            FortyfiveDegreeDirection::Left => Direction::LEFT,
            FortyfiveDegreeDirection::Left45 => Direction::LEFT45,
            FortyfiveDegreeDirection::Down => Direction::DOWN,
            FortyfiveDegreeDirection::Down45 => Direction::DOWN45,
        }
    }
}
