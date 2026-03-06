//! Configurable retry policies for Axon transitions.
//!
//! Provides `RetryPolicy` with fixed and exponential backoff strategies.

use std::time::Duration;

/// Configurable retry policy for individual Axon transitions.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (not counting the initial attempt).
    pub max_retries: u32,
    /// Backoff strategy between retries.
    pub backoff: BackoffStrategy,
}

/// Backoff strategy between retry attempts.
#[derive(Debug, Clone)]
pub enum BackoffStrategy {
    /// Fixed delay between retries.
    Fixed(Duration),
    /// Exponential backoff with configurable initial delay, multiplier, and max delay.
    Exponential {
        initial: Duration,
        multiplier: f64,
        max: Duration,
    },
}

impl RetryPolicy {
    /// Create a retry policy with fixed delay between attempts.
    pub fn fixed(max_retries: u32, delay: Duration) -> Self {
        Self {
            max_retries,
            backoff: BackoffStrategy::Fixed(delay),
        }
    }

    /// Create a retry policy with exponential backoff.
    ///
    /// # Arguments
    /// - `max_retries`: Maximum number of retry attempts
    /// - `initial`: Initial delay (e.g., 100ms)
    /// - `multiplier`: Backoff multiplier (e.g., 2.0 for doubling)
    /// - `max`: Maximum delay cap
    pub fn exponential(max_retries: u32, initial: Duration, multiplier: f64, max: Duration) -> Self {
        Self {
            max_retries,
            backoff: BackoffStrategy::Exponential {
                initial,
                multiplier,
                max,
            },
        }
    }

    /// Convenience: exponential backoff starting at `initial_ms` milliseconds,
    /// doubling each time, capped at 30 seconds.
    pub fn exponential_default(max_retries: u32, initial_ms: u64) -> Self {
        Self::exponential(
            max_retries,
            Duration::from_millis(initial_ms),
            2.0,
            Duration::from_secs(30),
        )
    }

    /// Calculate the delay for the given attempt number (0-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        match &self.backoff {
            BackoffStrategy::Fixed(d) => *d,
            BackoffStrategy::Exponential {
                initial,
                multiplier,
                max,
            } => {
                let delay_ms =
                    initial.as_millis() as f64 * multiplier.powi(attempt as i32);
                let delay = Duration::from_millis(delay_ms as u64);
                if delay > *max { *max } else { delay }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_delay_is_constant() {
        let policy = RetryPolicy::fixed(3, Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(100));
    }

    #[test]
    fn exponential_delay_doubles() {
        let policy = RetryPolicy::exponential(
            5,
            Duration::from_millis(100),
            2.0,
            Duration::from_secs(10),
        );
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(800));
    }

    #[test]
    fn exponential_delay_caps_at_max() {
        let policy = RetryPolicy::exponential(
            10,
            Duration::from_millis(100),
            2.0,
            Duration::from_millis(500),
        );
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(500)); // capped
        assert_eq!(policy.delay_for_attempt(4), Duration::from_millis(500)); // still capped
    }

    #[test]
    fn default_exponential_starts_correctly() {
        let policy = RetryPolicy::exponential_default(3, 100);
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
    }
}
