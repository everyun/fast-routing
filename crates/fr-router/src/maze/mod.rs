//! The maze search itself: frontier, node state, heuristic, drills, ripup and
//! the main loop.
//!
//! The module cut is `maze-engine.md` §13's ("Suggested Rust module cut"),
//! minus `rooms/`, which lives one level up in [`crate::rooms`].

pub mod drills;
pub mod heuristic;
pub mod nodes;
pub mod queue;
pub mod ripup;
pub mod search;
