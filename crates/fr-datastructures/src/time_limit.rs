//! Timeout and elapsed time tracking for performance-critical algorithms.
//!
//! Ported from `app.freerouting.datastructures.TimeLimit`.

use std::time::{Duration, Instant};

/// Monitors execution time and determines whether a configured time limit is exceeded.
#[derive(Debug, Clone, Copy)]
pub struct TimeLimit {
    start_time: Instant,
    limit: Duration,
}

impl TimeLimit {
    /// Creates a new `TimeLimit` with a limit specified in milliseconds.
    pub fn new(millis: u64) -> Self {
        Self::from_duration(Duration::from_millis(millis))
    }

    /// Creates a new `TimeLimit` from a `Duration`.
    pub fn from_duration(limit: Duration) -> Self {
        TimeLimit {
            start_time: Instant::now(),
            limit,
        }
    }

    /// Creates an unlimited `TimeLimit` that will never be exceeded.
    pub fn unlimited() -> Self {
        Self::from_duration(Duration::MAX)
    }

    /// Returns `true` if the time limit has been exceeded since creation/reset.
    pub fn limit_exceeded(&self) -> bool {
        self.start_time.elapsed() >= self.limit
    }

    /// Returns the elapsed duration since the timer started.
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Returns the elapsed milliseconds.
    pub fn elapsed_millis(&self) -> u64 {
        self.elapsed().as_millis() as u64
    }

    /// Returns the configured limit duration.
    pub fn limit(&self) -> Duration {
        self.limit
    }

    /// Returns the configured limit in milliseconds.
    pub fn limit_millis(&self) -> u64 {
        self.limit.as_millis() as u64
    }

    /// Returns the remaining duration before expiration, or zero if already exceeded.
    pub fn remaining(&self) -> Duration {
        let elapsed = self.elapsed();
        if elapsed >= self.limit {
            Duration::ZERO
        } else {
            self.limit - elapsed
        }
    }

    /// Resets the timer start time to now.
    pub fn reset(&mut self) {
        self.start_time = Instant::now();
    }

    /// Multiplies the remaining time limit by a positive factor.
    pub fn multiply(&mut self, factor: f64) {
        if factor <= 0.0 {
            return;
        }
        let current_secs = self.limit.as_secs_f64();
        let new_secs = current_secs * factor;
        if new_secs.is_finite() && new_secs >= 0.0 {
            self.limit = Duration::from_secs_f64(new_secs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_limit() {
        let timer = TimeLimit::new(5000);
        assert!(!timer.limit_exceeded());
        assert!(timer.remaining() > Duration::ZERO);

        let mut zero_timer = TimeLimit::new(0);
        assert!(zero_timer.limit_exceeded());

        zero_timer.multiply(100.0);
        assert_eq!(zero_timer.limit_millis(), 0);
    }
}
