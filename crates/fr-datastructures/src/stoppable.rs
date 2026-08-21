//! Cooperative cancellation interface for long-running algorithms and threads.
//!
//! Ported from `app.freerouting.datastructures.Stoppable`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Interface for stoppable algorithms, threads, or tasks.
pub trait Stoppable: Send + Sync {
    /// Requests that this operation or thread be stopped cooperatively.
    fn request_stop(&self);

    /// Returns `true` if a stop has been requested.
    fn is_stop_requested(&self) -> bool;
}

/// A thread-safe handle for requesting and querying cooperative stop status.
#[derive(Debug, Clone, Default)]
pub struct AtomicStoppable {
    stop_requested: Arc<AtomicBool>,
}

impl AtomicStoppable {
    /// Creates a new `AtomicStoppable` with stop requested set to `false`.
    pub fn new() -> Self {
        AtomicStoppable {
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Resets the stop request status to `false`.
    pub fn reset(&self) {
        self.stop_requested.store(false, Ordering::Relaxed);
    }
}

impl Stoppable for AtomicStoppable {
    fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
    }

    fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stoppable() {
        let stoppable = AtomicStoppable::new();
        assert!(!stoppable.is_stop_requested());

        let clone = stoppable.clone();
        clone.request_stop();

        assert!(stoppable.is_stop_requested());
        stoppable.reset();
        assert!(!stoppable.is_stop_requested());
    }
}
