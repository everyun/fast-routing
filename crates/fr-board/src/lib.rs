//! Board data model for the Freerouting Rust port.
//!
//! Mirrors `app.freerouting.board` from the upstream Java project:
//!
//! - `ItemHeader`, `FixedState` — base item identity and immutability flags.
//! - `Pin`, `Via`, `PolylineTrace`, `ObstacleArea`, `ConductionArea` — board items.
//! - `BasicBoard` — master PCB container.

pub mod area;
pub mod basic_board;
pub mod item;
pub mod pin;
pub mod trace;
pub mod via;

pub use area::{ConductionArea, ObstacleArea};
pub use basic_board::BasicBoard;
pub use item::{FixedState, ItemHeader, ItemType};
pub use pin::Pin;
pub use trace::PolylineTrace;
pub use via::Via;
