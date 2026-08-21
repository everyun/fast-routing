//! Planar computational geometry for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.geometry.planar` from the upstream Java project:
//!
//! - `IntPoint`, `RationalPoint`, `Point` — exact integer grid & rational coordinates.
//! - `IntVector`, `RationalVector`, `Vector` — 2D vector algebra and double dispatch.
//! - `Direction`, `FortyfiveDegreeDirection` — 45° angle routing directions.
//! - `IntBox`, `IntOctagon`, `Line`, `Simplex`, `Circle`, `Polyline`, `PolylineShape`, `Polygon` — board shapes.
//! - `FloatPoint`, `FloatLine` — double-precision approximations for metrics.

pub mod planar;

pub use planar::{
    Area, Circle, ConvexShape, Direction, FloatLine, FloatPoint, FortyfiveDegreeDirection, IntBox,
    IntOctagon, IntPoint, IntVector, Limits, Line, LineSegment, Point, Polygon, Polyline,
    PolylineShape, RationalPoint, RationalVector, Shape, Side, Simplex, Vector,
};
