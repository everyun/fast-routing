//! Planar computational geometry for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.geometry.planar` from the upstream Java project:
//!
//! - `IntPoint`, `RationalPoint`, `Point` — exact integer grid & rational coordinates.
//! - `IntVector`, `RationalVector`, `Vector` — 2D vector algebra and double dispatch.
//! - `Direction`, `FortyfiveDegreeDirection` — 45° angle routing directions.
//! - `IntBox`, `IntOctagon`, `Line`, `Simplex`, `Circle`, `Polyline`, `PolylineShape`, `Polygon` — board shapes.
//! - `FloatPoint`, `FloatLine` — double-precision approximations for metrics.
//!
//! [`util`] carries the one thing that is not a shape: `java.lang.Math`'s rounding, which every
//! `double` → integer coordinate in the kernel and in the importer goes through.

pub mod planar;
pub mod util;

pub use util::{jrint, jround, jround_i32, JavaRound};

pub use planar::{
    split_to_convex, Area, Circle, ConvexShape, Direction, FloatLine, FloatPoint,
    FortyfiveDegreeDirection, IntBox, IntOctagon, IntPoint, IntVector, Limits, Line, LineSegment,
    Point, Polygon, Polyline, PolylineShape, RationalPoint, RationalVector, Shape, Side, Simplex,
    Vector,
};
