//! Distributed rate limiting using Redis sliding window algorithm.
//!
//! Enable with the `distributed` feature flag:
//! ```toml
//! ranvier-guard = { version = "0.39", features = ["distributed"] }
//! ```
//!
//! Requires a running Redis instance. Set `REDIS_URL` environment variable
//! or pass the connection string directly.

use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::ClientIdentity;

/// Distributed rate limit guard using Redis sliding window.
///
/// Uses a sorted set per client with timestamps as scores.
/// On each request:
/// 1. Remove entries older than the window
/// 2. Count remaining entries
/// 3. If under limit, add current timestamp and allow
/// 4. If over limit, reject with retry-after
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_guard::DistributedRateLimitGuard;
/// use std::time::Duration;
///
/// let guard = DistributedRateLimitGuard::<String>::new(
///     "redis://127.0.0.1:6379",
///     100,                          // max requests
///     Duration::from_secs(60),      // per window
/// ).await.unwrap();
/// ```
pub struct DistributedRateLimitGuard<T> {
    connection: Arc<Mutex<redis::aio::MultiplexedConnection>>,
    max_requests: u64,
    window_ms: u64,
    key_prefix: String,
    _marker: PhantomData<T>,
}

impl<T> DistributedRateLimitGuard<T> {
    /// Create a new distributed rate limit guard.
    pub async fn new(
        redis_url: &str,
        max_requests: u64,
        window: std::time::Duration,
    ) -> Result<Self, String> {
        let client =
            redis::Client::open(redis_url).map_err(|e| format!("Redis connection error: {e}"))?;
        let conn = client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| format!("Redis connect error: {e}"))?;

        Ok(Self {
            connection: Arc::new(Mutex::new(conn)),
            max_requests,
            window_ms: window.as_millis() as u64,
            key_prefix: "ranvier:ratelimit:".to_string(),
            _marker: PhantomData,
        })
    }

    /// Set a custom key prefix for Redis keys (default: "ranvier:ratelimit:").
    pub fn with_key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Returns the max requests per window.
    pub fn max_requests(&self) -> u64 {
        self.max_requests
    }

    /// Returns the window duration in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }
}

impl<T> Clone for DistributedRateLimitGuard<T> {
    fn clone(&self) -> Self {
        Self {
            connection: self.connection.clone(),
            max_requests: self.max_requests,
            window_ms: self.window_ms,
            key_prefix: self.key_prefix.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for DistributedRateLimitGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DistributedRateLimitGuard")
            .field("max_requests", &self.max_requests)
            .field("window_ms", &self.window_ms)
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

#[async_trait]
impl<T> Transition<T, T> for DistributedRateLimitGuard<T>
where
    T: Send + Sync + 'static,
{
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: T,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<T, Self::Error> {
        let client_id = bus
            .read::<ClientIdentity>()
            .map(|c| c.0.clone())
            .unwrap_or_else(|| "anonymous".to_string());

        let key = format!("{}{}", self.key_prefix, client_id);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let window_start = now_ms.saturating_sub(self.window_ms);

        let mut conn = self.connection.lock().await;

        // Sliding window: use Redis pipeline for atomicity
        let result: Result<(u64, u64, bool), redis::RedisError> = redis::pipe()
            .atomic()
            // 1. Remove entries outside the window
            .cmd("ZREMRANGEBYSCORE")
            .arg(&key)
            .arg(0u64)
            .arg(window_start)
            .ignore()
            // 2. Count entries in the window
            .cmd("ZCARD")
            .arg(&key)
            // 3. Add current timestamp (member = unique via now_ms + random suffix)
            .cmd("ZADD")
            .arg(&key)
            .arg(now_ms)
            .arg(format!("{now_ms}:{}", uuid::Uuid::new_v4()))
            // 4. Set TTL on the key (auto-cleanup)
            .cmd("PEXPIRE")
            .arg(&key)
            .arg(self.window_ms + 1000) // window + 1s buffer
            .ignore()
            .query_async(&mut *conn)
            .await;

        match result {
            Ok((count, ..)) => {
                if count < self.max_requests {
                    Outcome::next(input)
                } else {
                    // Over limit — compute retry-after
                    let retry_after_ms = self.window_ms / self.max_requests;
                    Outcome::fault(format!(
                        "Rate limit exceeded (distributed). Retry after {retry_after_ms}ms"
                    ))
                }
            }
            Err(e) => {
                // Redis error — fail open (allow the request) but log warning
                tracing::warn!(error = %e, "Distributed rate limit Redis error — failing open");
                Outcome::next(input)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests require a running Redis instance.
    // Run with: REDIS_URL=redis://127.0.0.1:6379 cargo test -p ranvier-guard --features distributed

    #[tokio::test]
    async fn distributed_guard_connects_and_rate_limits() {
        let redis_url = match std::env::var("REDIS_URL") {
            Ok(url) => url,
            Err(_) => {
                eprintln!("REDIS_URL not set, skipping distributed rate limit test");
                return;
            }
        };

        let guard = DistributedRateLimitGuard::<String>::new(
            &redis_url,
            10,
            std::time::Duration::from_secs(60),
        )
        .await;

        assert!(guard.is_ok(), "Should connect to Redis");

        let guard = guard.unwrap();
        assert_eq!(guard.max_requests(), 10);
        assert_eq!(guard.window_ms(), 60000);

        // Test a single request
        let mut bus = Bus::new();
        bus.insert(ClientIdentity("test-distributed".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn distributed_guard_rejects_invalid_redis_url() {
        let result = DistributedRateLimitGuard::<String>::new(
            "redis://invalid-host-that-does-not-exist:9999",
            10,
            std::time::Duration::from_secs(60),
        )
        .await;

        // Connection should fail (DNS resolution or connection refused)
        assert!(result.is_err());
    }
}
