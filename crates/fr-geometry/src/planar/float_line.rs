//! Floating point line approximation, ported from
//! `app.freerouting.geometry.planar.FloatLine`.

use crate::planar::{float_point::FloatPoint, limits::Limits};

/// Defines a directed line in the plane by two `FloatPoint`s.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatLine {
    pub a: FloatPoint,
    pub b: FloatPoint,
}

impl FloatLine {
    /// Creates a line from two `FloatPoint`s.
    pub const fn new(a: FloatPoint, b: FloatPoint) -> Self {
        FloatLine { a, b }
    }

    /// Returns the `FloatLine` with swapped endpoints.
    pub fn opposite(&self) -> FloatLine {
        FloatLine::new(self.b, self.a)
    }

    /// Adjusts this line's direction to match another line.
    pub fn adjust_direction(&self, other: &FloatLine) -> FloatLine {
        if self.b.side_of(&self.a, &other.a) == other.b.side_of(&self.a, &other.a) {
            *self
        } else {
            self.opposite()
        }
    }

    /// Calculates the intersection with `other`. Returns `None` if parallel.
    pub fn intersection(&self, other: &FloatLine) -> Option<FloatPoint> {
        let d1x = self.b.x - self.a.x;
        let d1y = self.b.y - self.a.y;
        let d2x = other.b.x - other.a.x;
        let d2y = other.b.y - other.a.y;
        let det1 = self.a.x * self.b.y - self.a.y * self.b.x;
        let det2 = other.a.x * other.b.y - other.a.y * other.b.x;
        let det = d2x * d1y - d2y * d1x;
        if det == 0.0 {
            None
        } else {
            let is_x = (d2x * det1 - d1x * det2) / det;
            let is_y = (d2y * det1 - d1y * det2) / det;
            Some(FloatPoint::new(is_x, is_y))
        }
    }

    /// Translates the line perpendicular by `dist` (positive = left).
    pub fn translate(&self, dist: f64) -> FloatLine {
        let dx = self.b.x - self.a.x;
        let dy = self.b.y - self.a.y;
        let dxdx = dx * dx;
        let dydy = dy * dy;
        let length = (dxdx + dydy).sqrt();
        let new_a = if dxdx <= dydy {
            let rel_x = (dist * length) / dy;
            FloatPoint::new(self.a.x - rel_x, self.a.y)
        } else {
            let rel_y = (dist * length) / dx;
            FloatPoint::new(self.a.x, self.a.y + rel_y)
        };
        let new_b = FloatPoint::new(new_a.x + dx, new_a.y + dy);
        FloatLine::new(new_a, new_b)
    }

    /// Signed distance of this line from `point` (positive = left).
    pub fn signed_distance(&self, point: &FloatPoint) -> f64 {
        let dx = self.b.x - self.a.x;
        let dy = self.b.y - self.a.y;
        let det = dy * (point.x - self.a.x) - dx * (point.y - self.a.y);
        let length = (dx * dx + dy * dy).sqrt();
        det / length
    }

    /// Perpendicular projection of `point` onto this line.
    pub fn perpendicular_projection(&self, point: &FloatPoint) -> FloatPoint {
        let dx = self.b.x - self.a.x;
        let dy = self.b.y - self.a.y;
        if dx == 0.0 && dy == 0.0 {
            return self.a;
        }
        let dxdx = dx * dx;
        let dydy = dy * dy;
        let dxdy = dx * dy;
        let denominator = dxdx + dydy;
        let det = self.a.x * self.b.y - self.b.x * self.a.y;

        let x = (point.x * dxdx + point.y * dxdy + det * dy) / denominator;
        let y = (point.x * dxdy + point.y * dydy - det * dx) / denominator;
        FloatPoint::new(x, y)
    }

    /// Distance of `point` to the nearest point on the segment `[a, b]`.
    pub fn segment_distance(&self, point: &FloatPoint) -> f64 {
        let projection = self.perpendicular_projection(point);
        if projection.is_contained_in_box(&self.a, &self.b, 0.01) {
            point.distance(&projection)
        } else {
            point.distance(&self.a).min(point.distance(&self.b))
        }
    }

    /// Perpendicular projection of `line_segment` onto this oriented segment.
    pub fn segment_projection(&self, line_segment: &FloatLine) -> Option<FloatLine> {
        if self.b.scalar_product(&self.a, &line_segment.a) < 0.0 {
            return None;
        }
        if self.a.scalar_product(&self.b, &line_segment.b) < 0.0 {
            return None;
        }
        let projected_a = if self.a.scalar_product(&self.b, &line_segment.a) < 0.0 {
            self.a
        } else {
            let p = self.perpendicular_projection(&line_segment.a);
            if p.x.abs() >= Limits::CRIT_INT as f64 || p.y.abs() >= Limits::CRIT_INT as f64 {
                return None;
            }
            p
        };
        let projected_b = if self.b.scalar_product(&self.a, &line_segment.b) < 0.0 {
            self.b
        } else {
            self.perpendicular_projection(&line_segment.b)
        };
        if projected_b.x.abs() >= Limits::CRIT_INT as f64
            || projected_b.y.abs() >= Limits::CRIT_INT as f64
        {
            return None;
        }
        Some(FloatLine::new(projected_a, projected_b))
    }

    /// Nearest point on segment `[a, b]` to `from_point`.
    pub fn nearest_segment_point(&self, from_point: &FloatPoint) -> FloatPoint {
        let projection = self.perpendicular_projection(from_point);
        if projection.is_contained_in_box(&self.a, &self.b, 0.01) {
            projection
        } else if from_point.distance_square(&self.a) <= from_point.distance_square(&self.b) {
            self.a
        } else {
            self.b
        }
    }
}
