//! Abstract bounding shape traits and basic 2D bounding boxes for spatial trees.
//!
//! Ported from `app.freerouting.datastructures.ShapeTree`.

/// Trait implemented by geometric bounding shapes (boxes, octagons, etc.) used in spatial trees.
pub trait BoundingShape: Clone + Send + Sync {
    /// Returns `true` if this bounding shape intersects `other`.
    fn intersects(&self, other: &Self) -> bool;

    /// Returns the minimal bounding shape containing both `self` and `other`.
    fn union(&self, other: &Self) -> Self;

    /// Computes the area of this bounding shape for spatial expansion metrics.
    fn area(&self) -> f64;

    /// Returns `true` if this bounding shape fully contains `other`.
    fn contains(&self, other: &Self) -> bool;
}

/// An axis-aligned 2D integer bounding box implementing [`BoundingShape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundingBox2D {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

impl BoundingBox2D {
    /// Creates a new bounding box.
    pub const fn new(min_x: i32, min_y: i32, max_x: i32, max_y: i32) -> Self {
        BoundingBox2D {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    /// Creates a bounding box around a single point.
    pub const fn point(x: i32, y: i32) -> Self {
        BoundingBox2D {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
        }
    }

    /// Returns the width of the box.
    pub fn width(&self) -> i32 {
        self.max_x - self.min_x
    }

    /// Returns the height of the box.
    pub fn height(&self) -> i32 {
        self.max_y - self.min_y
    }
}

impl BoundingShape for BoundingBox2D {
    fn intersects(&self, other: &Self) -> bool {
        !(self.max_x < other.min_x
            || self.min_x > other.max_x
            || self.max_y < other.min_y
            || self.min_y > other.max_y)
    }

    fn union(&self, other: &Self) -> Self {
        BoundingBox2D {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    fn area(&self) -> f64 {
        let w = (self.max_x - self.min_x).max(0) as f64;
        let h = (self.max_y - self.min_y).max(0) as f64;
        w * h
    }

    fn contains(&self, other: &Self) -> bool {
        self.min_x <= other.min_x
            && self.min_y <= other.min_y
            && self.max_x >= other.max_x
            && self.max_y >= other.max_y
    }
}

/// Handle referencing a leaf entry in a spatial tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LeafId(pub usize);
