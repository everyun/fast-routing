//! Supporting data structures for the Freerouting Rust port.
//!
//! This crate mirrors `app.freerouting.datastructures` from the upstream Java
//! project. It contains small, dependency-light building blocks used across the
//! geometry, board and autorouting layers:
//!
//! - [`Signum`] — the mathematical sign function as a three-valued enum.
//! - [`BigIntAux`] — auxiliary `BigInteger` helpers (binary gcd, determinants, rational-coordinate arithmetic).
//! - [`ArrayStack`] — high-performance LIFO array stack.
//! - [`Stoppable`] / [`AtomicStoppable`] — cooperative cancellation interface and thread-safe implementation.
//! - [`TimeLimit`] — timeout and elapsed time tracker.
//! - [`IdGenerator`] / [`SequentialIdGenerator`] / [`AtomicIdGenerator`] — unique ID generator.
//! - [`IdentifierType`] — quoting and formatting rules for legal identifier names.
//! - [`IndentFileWriter`] — structured S-expression / parenthesis-based indented writer.
//! - [`UndoableObjects`] — transactional multi-level undo/redo state manager.
//! - [`BoundingShape`] / [`BoundingBox2D`] / [`LeafId`] — spatial bounding traits and handles.
//! - [`MinAreaTree`] — minimum-area hierarchical spatial tree for 2D range search and overlap queries.
//! - [`PlanarDelaunayTriangulation`] / [`Point2D`] / [`DelaunayEdge`] — 2D Delaunay triangulation and Minimum Spanning Tree (MST) computation.

pub mod array_stack;
pub mod big_int_aux;
pub mod id_generator;
pub mod identifier_type;
pub mod indent_file_writer;
pub mod min_area_tree;
pub mod planar_delaunay_triangulation;
pub mod shape_tree;
pub mod signum;
pub mod stoppable;
pub mod time_limit;
pub mod undoable_objects;

pub use array_stack::ArrayStack;
pub use big_int_aux::BigIntAux;
pub use id_generator::{AtomicIdGenerator, IdGenerator, SequentialIdGenerator};
pub use identifier_type::IdentifierType;
pub use indent_file_writer::IndentFileWriter;
pub use min_area_tree::MinAreaTree;
pub use planar_delaunay_triangulation::{DelaunayEdge, PlanarDelaunayTriangulation, Point2D};
pub use shape_tree::{BoundingBox2D, BoundingShape, LeafId};
pub use signum::Signum;
pub use stoppable::{AtomicStoppable, Stoppable};
pub use time_limit::TimeLimit;
pub use undoable_objects::{UndoableNode, UndoableObjects};
