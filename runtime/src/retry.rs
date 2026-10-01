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
    pub fn exponential(
        max_retries: u32,
        initial: Duration,
        multiplier: f64,
        max: Duration,
    ) -> Self {
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

    /// Calculate the delay for the given retry number (0-indexed).
    /// Exponential multipliers must be finite and positive; invalid values
    /// return the configured cap rather than silently producing a zero delay.
    /// A delay cap does not impose a total retry or effect-replay budget.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        match &self.backoff {
            BackoffStrategy::Fixed(d) => *d,
            BackoffStrategy::Exponential {
                initial,
                multiplier,
                max,
            } => {
                if !multiplier.is_finite() || *multiplier <= 0.0 {
                    return *max;
                }
                if attempt == 0 || initial.is_zero() {
                    return (*initial).min(*max);
                }
                let seconds = initial.as_secs_f64() * multiplier.powf(f64::from(attempt));
                if !seconds.is_finite() || seconds >= max.as_secs_f64() {
                    return *max;
                }
                Duration::try_from_secs_f64(seconds)
                    .unwrap_or(*max)
                    .min(*max)
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
        let policy =
            RetryPolicy::exponential(5, Duration::from_millis(100), 2.0, Duration::from_secs(10));
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
    fn exponential_large_attempts_do_not_wrap_negative() {
        let cap = Duration::from_secs(30);
        let policy = RetryPolicy::exponential(u32::MAX, Duration::from_millis(100), 2.0, cap);
        for attempt in [i32::MAX as u32, i32::MAX as u32 + 1, u32::MAX] {
            assert_eq!(policy.delay_for_attempt(attempt), cap);
        }
    }

    #[test]
    fn exponential_retains_submillisecond_precision_and_zero() {
        let policy =
            RetryPolicy::exponential(3, Duration::from_nanos(125), 2.0, Duration::from_secs(1));
        assert_eq!(policy.delay_for_attempt(0), Duration::from_nanos(125));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_nanos(250));
        let zero = RetryPolicy::exponential(3, Duration::ZERO, 2.0, Duration::from_secs(1));
        assert_eq!(zero.delay_for_attempt(u32::MAX), Duration::ZERO);
    }

    #[test]
    fn exponential_invalid_multiplier_returns_cap() {
        let cap = Duration::from_secs(1);
        for multiplier in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -2.0] {
            let policy = RetryPolicy::exponential(3, Duration::from_millis(100), multiplier, cap);
            assert_eq!(policy.delay_for_attempt(0), cap);
            assert_eq!(policy.delay_for_attempt(u32::MAX), cap);
        }
    }

    #[test]
    fn default_exponential_starts_correctly() {
        let policy = RetryPolicy::exponential_default(3, 100);
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
    }
}
