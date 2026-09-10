//! Snapshot generator stub for the GUI render model.
//!
//! Defined per `docs/tasks/GUI-CONTRACT.md`. `gui-render` will implement full conversion.

pub use fr_router::snapshot::*;

/// Builds a snapshot of the board state for GUI rendering.
///
/// This is a stub implementation returning an empty render model with stats.
/// `gui-render` provides the production implementation.
pub fn snapshot(_board: &fr_board::Board, stats: Stats) -> RenderModel {
    RenderModel {
        layers: Vec::new(),
        bbox: [0.0; 4],
        resolution: 1.0,
        traces: Vec::new(),
        vias: Vec::new(),
        pads: Vec::new(),
        areas: Vec::new(),
        ratsnest: Vec::new(),
        stats,
    }
}
