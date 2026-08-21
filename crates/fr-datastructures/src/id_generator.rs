//! Unique identification number generators.
//!
//! Ported from `app.freerouting.datastructures.IdentificationNumberGenerator`.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

/// Interface for generating unique identification numbers.
pub trait IdGenerator {
    /// Generates and returns a new unique identification number.
    fn new_no(&mut self) -> i32;

    /// Returns the maximum generated identification number so far.
    fn max_generated_no(&self) -> i32;
}

/// A sequential single-threaded identification number generator.
#[derive(Debug, Clone, Default)]
pub struct SequentialIdGenerator {
    current: i32,
}

impl SequentialIdGenerator {
    /// Creates a new generator starting at 0 (first generated number is 1).
    pub fn new() -> Self {
        SequentialIdGenerator { current: 0 }
    }

    /// Creates a new generator with a custom initial value.
    pub fn with_start(start: i32) -> Self {
        SequentialIdGenerator { current: start }
    }

    /// Resets the generator counter to `start`.
    pub fn reset(&mut self, start: i32) {
        self.current = start;
    }
}

impl IdGenerator for SequentialIdGenerator {
    fn new_no(&mut self) -> i32 {
        self.current += 1;
        self.current
    }

    fn max_generated_no(&self) -> i32 {
        self.current
    }
}

/// A thread-safe atomic identification number generator.
#[derive(Debug, Clone, Default)]
pub struct AtomicIdGenerator {
    current: Arc<AtomicI32>,
}

impl AtomicIdGenerator {
    /// Creates a new atomic generator starting at 0.
    pub fn new() -> Self {
        AtomicIdGenerator {
            current: Arc::new(AtomicI32::new(0)),
        }
    }

    /// Creates a new atomic generator with a custom starting value.
    pub fn with_start(start: i32) -> Self {
        AtomicIdGenerator {
            current: Arc::new(AtomicI32::new(start)),
        }
    }

    /// Generates and returns a new unique identification number atomically.
    pub fn next_id(&self) -> i32 {
        self.current.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Returns the maximum generated identification number so far.
    pub fn max_id(&self) -> i32 {
        self.current.load(Ordering::Relaxed)
    }

    /// Resets the generator counter to `start`.
    pub fn reset(&self, start: i32) {
        self.current.store(start, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequential_generator() {
        let mut gen = SequentialIdGenerator::new();
        assert_eq!(gen.max_generated_no(), 0);
        assert_eq!(gen.new_no(), 1);
        assert_eq!(gen.new_no(), 2);
        assert_eq!(gen.max_generated_no(), 2);

        gen.reset(10);
        assert_eq!(gen.new_no(), 11);
    }

    #[test]
    fn test_atomic_generator() {
        let gen = AtomicIdGenerator::new();
        assert_eq!(gen.max_id(), 0);
        assert_eq!(gen.next_id(), 1);
        assert_eq!(gen.next_id(), 2);
        assert_eq!(gen.max_id(), 2);
    }
}
