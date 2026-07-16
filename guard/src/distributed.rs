//! Distributed rate limiting using Redis sliding window algorithm.
//!
//! Enable with the `distributed` feature flag:
//! ```toml
//! ranvier-guard = { version = "0.51", features = ["distributed"] }
//! ```
//!
//! Requires a running Redis instance. Set `REDIS_URL` environment variable
//! or pass the connection string directly.

use async_trait::async_trait;
use ranvier_core::runtime_policy::{
    PolicyComponent, PolicyField, PolicyObservation, PolicyValue, RuntimeProfile,
    StartupPolicyCode, StartupPolicyContribution, StartupPolicyProvider,
};
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Mutex;

use crate::{ClientIdentity, duration_millis_saturating};

/// Behavior when the distributed rate-limit backend is unavailable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DistributedRateLimitFailureMode {
    /// Preserve availability by allowing the request and logging the backend error.
    #[default]
    FailOpen,
    /// Preserve the rate-limit guarantee by rejecting the request.
    FailClosed,
}

/// Side-effect-free distributed rate-limit configuration.
///
/// Validate this value through `ResolvedRuntimeConfig::validate_startup`
/// before calling [`DistributedRateLimitGuard::connect`]. Its `Debug`
/// representation intentionally redacts the Redis URL and key prefix.
#[derive(Clone)]
pub struct DistributedRateLimitConfig {
    redis_url: String,
    max_requests: u64,
    window_ms: u64,
    key_prefix: String,
    failure_mode: Option<DistributedRateLimitFailureMode>,
}

impl DistributedRateLimitConfig {
    pub fn new(
        redis_url: impl Into<String>,
        max_requests: u64,
        window: std::time::Duration,
    ) -> Self {
        Self {
            redis_url: redis_url.into(),
            max_requests,
            window_ms: duration_millis_saturating(window),
            key_prefix: "ranvier:ratelimit:".to_string(),
            failure_mode: None,
        }
    }

    pub fn with_key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    pub fn with_failure_mode(mut self, failure_mode: DistributedRateLimitFailureMode) -> Self {
        self.failure_mode = Some(failure_mode);
        self
    }

    pub fn max_requests(&self) -> u64 {
        self.max_requests
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    pub fn failure_mode(&self) -> Option<DistributedRateLimitFailureMode> {
        self.failure_mode
    }
}

impl std::fmt::Debug for DistributedRateLimitConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DistributedRateLimitConfig")
            .field("redis_url", &"<redacted>")
            .field("max_requests", &self.max_requests)
            .field("window_ms", &self.window_ms)
            .field("key_prefix", &"<redacted>")
            .field("failure_mode", &self.failure_mode)
            .finish()
    }
}

impl StartupPolicyProvider for DistributedRateLimitConfig {
    fn startup_policy(&self, profile: RuntimeProfile) -> StartupPolicyContribution {
        distributed_startup_policy(
            profile,
            self.max_requests,
            self.window_ms,
            !self.key_prefix.trim().is_empty(),
            redis::Client::open(self.redis_url.as_str()).is_ok(),
            self.failure_mode.unwrap_or_default(),
            self.failure_mode.is_some(),
        )
    }
}

/// Observable backend and request outcomes for distributed rate limiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DistributedRateLimitStats {
    backend_available: bool,
    backend_errors: u64,
    backend_recoveries: u64,
    allowed_requests: u64,
    limited_requests: u64,
    bypassed_requests: u64,
}

impl DistributedRateLimitStats {
    pub const fn backend_available(self) -> bool {
        self.backend_available
    }

    pub const fn backend_errors(self) -> u64 {
        self.backend_errors
    }

    pub const fn backend_recoveries(self) -> u64 {
        self.backend_recoveries
    }

    pub const fn allowed_requests(self) -> u64 {
        self.allowed_requests
    }

    pub const fn limited_requests(self) -> u64 {
        self.limited_requests
    }

    pub const fn bypassed_requests(self) -> u64 {
        self.bypassed_requests
    }
}

#[derive(Debug)]
struct DistributedRateLimitMetrics {
    backend_available: AtomicBool,
    backend_errors: AtomicU64,
    backend_recoveries: AtomicU64,
    allowed_requests: AtomicU64,
    limited_requests: AtomicU64,
    bypassed_requests: AtomicU64,
}

impl DistributedRateLimitMetrics {
    fn connected() -> Self {
        Self {
            backend_available: AtomicBool::new(true),
            backend_errors: AtomicU64::new(0),
            backend_recoveries: AtomicU64::new(0),
            allowed_requests: AtomicU64::new(0),
            limited_requests: AtomicU64::new(0),
            bypassed_requests: AtomicU64::new(0),
        }
    }

    fn record_backend_error(&self, failure_mode: DistributedRateLimitFailureMode) {
        self.backend_available.store(false, Ordering::Relaxed);
        self.backend_errors.fetch_add(1, Ordering::Relaxed);
        if failure_mode == DistributedRateLimitFailureMode::FailOpen {
            self.bypassed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_backend_success(&self) {
        if !self.backend_available.swap(true, Ordering::Relaxed) {
            self.backend_recoveries.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> DistributedRateLimitStats {
        DistributedRateLimitStats {
            backend_available: self.backend_available.load(Ordering::Relaxed),
            backend_errors: self.backend_errors.load(Ordering::Relaxed),
            backend_recoveries: self.backend_recoveries.load(Ordering::Relaxed),
            allowed_requests: self.allowed_requests.load(Ordering::Relaxed),
            limited_requests: self.limited_requests.load(Ordering::Relaxed),
            bypassed_requests: self.bypassed_requests.load(Ordering::Relaxed),
        }
    }
}

struct RedisBackend {
    client: redis::Client,
    connection: Option<redis::aio::MultiplexedConnection>,
}

fn distributed_startup_policy(
    profile: RuntimeProfile,
    max_requests_value: u64,
    window_ms_value: u64,
    key_prefix_configured_value: bool,
    redis_url_valid_value: bool,
    failure_mode_value: DistributedRateLimitFailureMode,
    failure_mode_explicit_value: bool,
) -> StartupPolicyContribution {
    let component = PolicyComponent::new("guard.distributed_rate_limit");
    let max_requests = PolicyField::new("max_requests");
    let window_ms = PolicyField::new("window_ms");
    let key_prefix_configured = PolicyField::new("key_prefix_configured");
    let redis_url_valid = PolicyField::new("redis_url_valid");
    let failure_mode = PolicyField::new("failure_mode");
    let failure_mode_explicit = PolicyField::new("failure_mode_explicit");
    let mut violations = Vec::new();
    if max_requests_value == 0 {
        violations.push((StartupPolicyCode::ConfigValueInvalid, max_requests));
    }
    if window_ms_value == 0 {
        violations.push((StartupPolicyCode::ConfigValueInvalid, window_ms));
    }
    if !key_prefix_configured_value {
        violations.push((StartupPolicyCode::ConfigValueInvalid, key_prefix_configured));
    }
    if !redis_url_valid_value {
        violations.push((StartupPolicyCode::ConfigValueInvalid, redis_url_valid));
    }
    if profile == RuntimeProfile::Production && !failure_mode_explicit_value {
        violations.push((
            StartupPolicyCode::DistributedFailureModeUnset,
            failure_mode_explicit,
        ));
    }

    StartupPolicyContribution::new(
        component,
        vec![
            PolicyObservation::new(max_requests, PolicyValue::Count(max_requests_value)),
            PolicyObservation::new(window_ms, PolicyValue::DurationMs(window_ms_value)),
            PolicyObservation::new(
                key_prefix_configured,
                PolicyValue::Configured(key_prefix_configured_value),
            ),
            PolicyObservation::new(redis_url_valid, PolicyValue::Bool(redis_url_valid_value)),
            PolicyObservation::new(
                failure_mode,
                PolicyValue::Label(match failure_mode_value {
                    DistributedRateLimitFailureMode::FailOpen => "fail_open",
                    DistributedRateLimitFailureMode::FailClosed => "fail_closed",
                }),
            ),
            PolicyObservation::new(
                failure_mode_explicit,
                PolicyValue::Bool(failure_mode_explicit_value),
            ),
        ],
        violations,
    )
}

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
/// use ranvier_core::config::ResolvedRuntimeConfig;
/// use ranvier_core::runtime_policy::RuntimeProfile;
/// use ranvier_guard::{
///     DistributedRateLimitConfig, DistributedRateLimitFailureMode,
///     DistributedRateLimitGuard,
/// };
/// use std::time::Duration;
///
/// let resolved = ResolvedRuntimeConfig::load_for(RuntimeProfile::Production)?;
/// let config = DistributedRateLimitConfig::new(
///     "redis://127.0.0.1:6379",
///     100,                          // max requests
///     Duration::from_secs(60),      // per window
/// ).with_failure_mode(DistributedRateLimitFailureMode::FailClosed);
/// resolved.validate_startup(&[&config])?;
/// let guard = DistributedRateLimitGuard::<String>::connect(config).await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct DistributedRateLimitGuard<T> {
    backend: Arc<Mutex<RedisBackend>>,
    max_requests: u64,
    window_ms: u64,
    key_prefix: String,
    failure_mode: DistributedRateLimitFailureMode,
    failure_mode_explicit: bool,
    metrics: Arc<DistributedRateLimitMetrics>,
    _marker: PhantomData<T>,
}

impl<T> DistributedRateLimitGuard<T> {
    /// Create a legacy distributed rate-limit guard.
    ///
    /// This preserves the historical implicit fail-open mode. Production-aware
    /// startup should validate [`DistributedRateLimitConfig`] first and then
    /// call [`Self::connect`].
    pub async fn new(
        redis_url: &str,
        max_requests: u64,
        window: std::time::Duration,
    ) -> Result<Self, String> {
        Self::connect_inner(
            DistributedRateLimitConfig::new(redis_url, max_requests, window),
            false,
        )
        .await
    }

    /// Connect only after the supplied configuration has passed startup-policy
    /// validation. A missing explicit failure mode remains visible in the
    /// configuration's Production policy report.
    pub async fn connect(config: DistributedRateLimitConfig) -> Result<Self, String> {
        let failure_mode_explicit = config.failure_mode.is_some();
        Self::connect_inner(config, failure_mode_explicit).await
    }

    async fn connect_inner(
        config: DistributedRateLimitConfig,
        failure_mode_explicit: bool,
    ) -> Result<Self, String> {
        let client = redis::Client::open(config.redis_url.as_str())
            .map_err(|error| format!("Redis connection configuration error: {error}"))?;
        let conn = client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|error| format!("Redis connect error: {error}"))?;

        Ok(Self {
            backend: Arc::new(Mutex::new(RedisBackend {
                client,
                connection: Some(conn),
            })),
            max_requests: config.max_requests,
            window_ms: config.window_ms,
            key_prefix: config.key_prefix,
            failure_mode: config.failure_mode.unwrap_or_default(),
            failure_mode_explicit,
            metrics: Arc::new(DistributedRateLimitMetrics::connected()),
            _marker: PhantomData,
        })
    }

    /// Set a custom key prefix for Redis keys (default: "ranvier:ratelimit:").
    pub fn with_key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Set behavior when Redis is unavailable.
    ///
    /// The default is [`DistributedRateLimitFailureMode::FailOpen`] for
    /// backwards compatibility. Production services that treat rate limiting as
    /// a security control should choose
    /// [`DistributedRateLimitFailureMode::FailClosed`].
    pub fn with_failure_mode(mut self, failure_mode: DistributedRateLimitFailureMode) -> Self {
        self.failure_mode = failure_mode;
        self.failure_mode_explicit = true;
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

    /// Returns Redis dependency failure behavior.
    pub fn failure_mode(&self) -> DistributedRateLimitFailureMode {
        self.failure_mode
    }

    /// Returns whether the dependency failure mode was selected explicitly.
    pub fn failure_mode_explicit(&self) -> bool {
        self.failure_mode_explicit
    }

    /// Returns backend health transitions and request outcome counters.
    pub fn stats(&self) -> DistributedRateLimitStats {
        self.metrics.snapshot()
    }

    fn redis_error_outcome(
        input: T,
        failure_mode: DistributedRateLimitFailureMode,
        error: &redis::RedisError,
    ) -> Outcome<T, String> {
        match failure_mode {
            DistributedRateLimitFailureMode::FailOpen => {
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "Distributed rate limit Redis error; failing open"
                );
                Outcome::next(input)
            }
            DistributedRateLimitFailureMode::FailClosed => {
                tracing::error!(
                    error_kind = ?error.kind(),
                    "Distributed rate limit Redis error; failing closed"
                );
                Outcome::fault("Distributed rate limit backend unavailable".to_string())
            }
        }
    }
}

impl<T> StartupPolicyProvider for DistributedRateLimitGuard<T> {
    fn startup_policy(&self, profile: RuntimeProfile) -> StartupPolicyContribution {
        distributed_startup_policy(
            profile,
            self.max_requests,
            self.window_ms,
            !self.key_prefix.trim().is_empty(),
            true,
            self.failure_mode,
            self.failure_mode_explicit,
        )
    }
}

impl<T> Clone for DistributedRateLimitGuard<T> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            max_requests: self.max_requests,
            window_ms: self.window_ms,
            key_prefix: self.key_prefix.clone(),
            failure_mode: self.failure_mode,
            failure_mode_explicit: self.failure_mode_explicit,
            metrics: self.metrics.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for DistributedRateLimitGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DistributedRateLimitGuard")
            .field("max_requests", &self.max_requests)
            .field("window_ms", &self.window_ms)
            .field("key_prefix", &"<redacted>")
            .field("failure_mode", &self.failure_mode)
            .field("failure_mode_explicit", &self.failure_mode_explicit)
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

        let mut backend = self.backend.lock().await;
        if backend.connection.is_none() {
            match backend.client.get_multiplexed_tokio_connection().await {
                Ok(connection) => backend.connection = Some(connection),
                Err(error) => {
                    drop(backend);
                    self.metrics.record_backend_error(self.failure_mode);
                    return Self::redis_error_outcome(input, self.failure_mode, &error);
                }
            }
        }

        let result = match backend.connection.as_mut() {
            Some(connection) => {
                redis::Script::new(
                    r#"
redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, ARGV[1])
local count = redis.call('ZCARD', KEYS[1])
if count < tonumber(ARGV[2]) then
  redis.call('ZADD', KEYS[1], ARGV[3], ARGV[4])
  redis.call('PEXPIRE', KEYS[1], ARGV[5])
  return {count, 1}
end
return {count, 0}
"#,
                )
                .key(&key)
                .arg(window_start)
                .arg(self.max_requests)
                .arg(now_ms)
                .arg(format!("{now_ms}:{}", uuid::Uuid::new_v4()))
                .arg(self.window_ms.saturating_add(1_000))
                .invoke_async::<(u64, u64)>(connection)
                .await
            }
            None => Err(redis::RedisError::from((
                redis::ErrorKind::IoError,
                "Redis connection unavailable after reconnect",
            ))),
        };
        if result.is_err() {
            backend.connection = None;
        }
        drop(backend);

        match result {
            Ok((_count, 1)) => {
                self.metrics.record_backend_success();
                self.metrics
                    .allowed_requests
                    .fetch_add(1, Ordering::Relaxed);
                Outcome::next(input)
            }
            Ok((_count, _)) => {
                self.metrics.record_backend_success();
                self.metrics
                    .limited_requests
                    .fetch_add(1, Ordering::Relaxed);
                let retry_after_ms = self.window_ms / self.max_requests.max(1);
                Outcome::fault(format!(
                    "Rate limit exceeded (distributed). Retry after {retry_after_ms}ms"
                ))
            }
            Err(error) => {
                self.metrics.record_backend_error(self.failure_mode);
                Self::redis_error_outcome(input, self.failure_mode, &error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(profile: RuntimeProfile) -> ranvier_core::config::ResolvedRuntimeConfig {
        let path = std::env::temp_dir().join(format!(
            "ranvier-distributed-guard-policy-{}.toml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "").unwrap();
        let resolved =
            ranvier_core::config::ResolvedRuntimeConfig::from_file_for(&path, profile).unwrap();
        std::fs::remove_file(path).unwrap();
        resolved
    }

    fn redis_error() -> redis::RedisError {
        redis::RedisError::from((redis::ErrorKind::IoError, "simulated Redis backend failure"))
    }

    #[test]
    fn redis_error_outcome_fails_open_by_default() {
        let outcome = DistributedRateLimitGuard::<String>::redis_error_outcome(
            "ok".to_string(),
            DistributedRateLimitFailureMode::default(),
            &redis_error(),
        );

        assert!(matches!(outcome, Outcome::Next(value) if value == "ok"));
    }

    #[test]
    fn redis_error_outcome_can_fail_closed() {
        let outcome = DistributedRateLimitGuard::<String>::redis_error_outcome(
            "ok".to_string(),
            DistributedRateLimitFailureMode::FailClosed,
            &redis_error(),
        );

        assert!(
            matches!(outcome, Outcome::Fault(message) if message.contains("backend unavailable"))
        );
    }

    #[test]
    fn production_policy_requires_explicit_distributed_failure_mode() {
        let config = DistributedRateLimitConfig::new(
            "redis://user:sentinel-secret@127.0.0.1:6379",
            10,
            std::time::Duration::from_secs(60),
        );
        let error = resolved(RuntimeProfile::Production)
            .validate_startup(&[&config])
            .unwrap_err();

        assert_eq!(
            error.report().violation_codes().collect::<Vec<_>>(),
            vec![StartupPolicyCode::DistributedFailureModeUnset]
        );
        let report = serde_json::to_string(error.report()).unwrap();
        assert!(report.contains("guard.distributed_rate_limit"));
        assert!(report.contains("failure_mode_explicit"));
        assert!(report.contains("fail_open"));
        assert!(!report.contains("sentinel-secret"));
        assert!(!format!("{config:?}").contains("sentinel-secret"));
    }

    #[test]
    fn explicit_fail_open_or_closed_passes_production_policy() {
        for failure_mode in [
            DistributedRateLimitFailureMode::FailOpen,
            DistributedRateLimitFailureMode::FailClosed,
        ] {
            let config = DistributedRateLimitConfig::new(
                "redis://127.0.0.1:6379",
                10,
                std::time::Duration::from_secs(60),
            )
            .with_failure_mode(failure_mode);

            assert!(
                resolved(RuntimeProfile::Production)
                    .validate_startup(&[&config])
                    .is_ok()
            );
        }
    }

    #[test]
    fn startup_policy_aggregates_heterogeneous_guard_providers() {
        let local = crate::RateLimitGuard::<String>::new(10, 1_000)
            .with_bucket_ttl(std::time::Duration::from_secs(300));
        let distributed = DistributedRateLimitConfig::new(
            "redis://127.0.0.1:6379",
            10,
            std::time::Duration::from_secs(60),
        )
        .with_failure_mode(DistributedRateLimitFailureMode::FailClosed);

        assert!(
            resolved(RuntimeProfile::Production)
                .validate_startup(&[&local, &distributed])
                .is_ok()
        );
    }

    #[test]
    fn backend_health_records_outage_and_single_recovery_transition() {
        let metrics = DistributedRateLimitMetrics::connected();
        metrics.record_backend_error(DistributedRateLimitFailureMode::FailClosed);
        let outage = metrics.snapshot();
        assert!(!outage.backend_available());
        assert_eq!(outage.backend_errors(), 1);
        assert_eq!(outage.bypassed_requests(), 0);

        metrics.record_backend_success();
        metrics.record_backend_success();
        let recovered = metrics.snapshot();
        assert!(recovered.backend_available());
        assert_eq!(recovered.backend_recoveries(), 1);

        let fail_open_metrics = DistributedRateLimitMetrics::connected();
        fail_open_metrics.record_backend_error(DistributedRateLimitFailureMode::FailOpen);
        assert_eq!(fail_open_metrics.snapshot().bypassed_requests(), 1);
    }

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
        assert_eq!(guard.stats().allowed_requests(), 1);
    }

    #[tokio::test]
    async fn denied_requests_do_not_grow_the_redis_window() {
        let redis_url = match std::env::var("REDIS_URL") {
            Ok(url) => url,
            Err(_) => return,
        };
        let suffix = uuid::Uuid::new_v4().to_string();
        let key_prefix = format!("ranvier:test:ratelimit:{suffix}:");
        let client_id = "bounded-client";
        let key = format!("{key_prefix}{client_id}");
        let guard = DistributedRateLimitGuard::<String>::connect(
            DistributedRateLimitConfig::new(
                redis_url.clone(),
                2,
                std::time::Duration::from_secs(60),
            )
            .with_key_prefix(key_prefix)
            .with_failure_mode(DistributedRateLimitFailureMode::FailClosed),
        )
        .await
        .unwrap();
        let mut bus = Bus::new();
        bus.insert(ClientIdentity(client_id.to_string()));

        for request in 0..5 {
            let outcome = guard.run(request.to_string(), &(), &mut bus).await;
            assert_eq!(matches!(outcome, Outcome::Next(_)), request < 2);
        }

        let client = redis::Client::open(redis_url.as_str()).unwrap();
        let mut connection = client.get_multiplexed_tokio_connection().await.unwrap();
        let cardinality: u64 = redis::cmd("ZCARD")
            .arg(&key)
            .query_async(&mut connection)
            .await
            .unwrap();
        let _: u64 = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut connection)
            .await
            .unwrap();

        assert_eq!(cardinality, 2);
        assert_eq!(guard.stats().allowed_requests(), 2);
        assert_eq!(guard.stats().limited_requests(), 3);
    }

    #[tokio::test]
    async fn distributed_guard_recovers_after_connection_outage() {
        let redis_url = match std::env::var("REDIS_URL") {
            Ok(url) => url,
            Err(_) => return,
        };
        let guard = DistributedRateLimitGuard::<String>::connect(
            DistributedRateLimitConfig::new(
                redis_url.clone(),
                10,
                std::time::Duration::from_secs(60),
            )
            .with_failure_mode(DistributedRateLimitFailureMode::FailClosed),
        )
        .await
        .unwrap();

        let mut bus = Bus::new();
        bus.insert(ClientIdentity(format!("recovery-{}", uuid::Uuid::new_v4())));
        {
            let mut backend = guard.backend.lock().await;
            backend.connection = None;
            backend.client = redis::Client::open("redis://127.0.0.1:1").unwrap();
        }
        let outage_result = guard.run("outage".into(), &(), &mut bus).await;
        assert!(matches!(outage_result, Outcome::Fault(_)));
        assert!(!guard.stats().backend_available());
        assert_eq!(guard.stats().backend_errors(), 1);

        {
            let mut backend = guard.backend.lock().await;
            backend.connection = None;
            backend.client = redis::Client::open(redis_url.as_str()).unwrap();
        }
        let recovery_result = guard.run("recovered".into(), &(), &mut bus).await;

        assert!(matches!(recovery_result, Outcome::Next(_)));
        assert!(guard.stats().backend_available());
        assert_eq!(guard.stats().backend_recoveries(), 1);
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
