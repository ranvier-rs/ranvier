//! Guard nodes — HTTP security/policy Transition nodes replacing Tower middleware.
//!
//! These nodes are designed to run early in a Schematic pipeline, enforcing
//! security policies as visible, traceable Transition steps rather than hidden
//! middleware layers.
//!
//! Each guard reads context from the Bus (e.g., request headers, client IP)
//! and either passes the input through or returns a Fault.

use async_trait::async_trait;
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// CorsGuard
// ---------------------------------------------------------------------------

/// CORS guard configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub max_age_seconds: u64,
    pub allow_credentials: bool,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["*".to_string()],
            allowed_methods: vec![
                "GET".into(),
                "POST".into(),
                "PUT".into(),
                "DELETE".into(),
                "OPTIONS".into(),
            ],
            allowed_headers: vec!["Content-Type".into(), "Authorization".into()],
            max_age_seconds: 86400,
            allow_credentials: false,
        }
    }
}

impl CorsConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }
}

/// Bus-injectable type representing the request origin header.
#[derive(Debug, Clone)]
pub struct RequestOrigin(pub String);

/// CORS guard Transition — validates the request origin against allowed origins.
///
/// Reads `RequestOrigin` from the Bus. If the origin is not allowed, returns Fault.
/// Writes CORS response headers to the Bus as `CorsHeaders`.
#[derive(Debug, Clone)]
pub struct CorsGuard<T> {
    config: CorsConfig,
    _marker: PhantomData<T>,
}

impl<T> CorsGuard<T> {
    pub fn new(config: CorsConfig) -> Self {
        Self {
            config,
            _marker: PhantomData,
        }
    }
}

/// CORS headers to be applied to the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsHeaders {
    pub access_control_allow_origin: String,
    pub access_control_allow_methods: String,
    pub access_control_allow_headers: String,
    pub access_control_max_age: String,
}

#[async_trait]
impl<T> Transition<T, T> for CorsGuard<T>
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
        let origin = bus
            .read::<RequestOrigin>()
            .map(|o| o.0.clone())
            .unwrap_or_default();

        let allowed = self.config.allowed_origins.contains(&"*".to_string())
            || self.config.allowed_origins.contains(&origin);

        if !allowed && !origin.is_empty() {
            return Outcome::fault(format!("CORS: origin '{}' not allowed", origin));
        }

        let allow_origin = if self.config.allowed_origins.contains(&"*".to_string()) {
            "*".to_string()
        } else {
            origin
        };

        bus.insert(CorsHeaders {
            access_control_allow_origin: allow_origin,
            access_control_allow_methods: self.config.allowed_methods.join(", "),
            access_control_allow_headers: self.config.allowed_headers.join(", "),
            access_control_max_age: self.config.max_age_seconds.to_string(),
        });

        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// RateLimitGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the client identity for rate limiting.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ClientIdentity(pub String);

/// Rate limit error with retry-after information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitError {
    pub message: String,
    pub retry_after_ms: u64,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (retry after {}ms)", self.message, self.retry_after_ms)
    }
}

/// Simple token-bucket rate limiter state.
struct RateBucket {
    tokens: f64,
    last_refill: Instant,
}

/// Rate limit guard — enforces per-client request rate limits.
///
/// Reads `ClientIdentity` from the Bus. Uses a token-bucket algorithm.
pub struct RateLimitGuard<T> {
    max_requests: u64,
    window_ms: u64,
    buckets: Arc<Mutex<std::collections::HashMap<String, RateBucket>>>,
    _marker: PhantomData<T>,
}

impl<T> RateLimitGuard<T> {
    pub fn new(max_requests: u64, window_ms: u64) -> Self {
        Self {
            max_requests,
            window_ms,
            buckets: Arc::new(Mutex::new(std::collections::HashMap::new())),
            _marker: PhantomData,
        }
    }
}

impl<T> Clone for RateLimitGuard<T> {
    fn clone(&self) -> Self {
        Self {
            max_requests: self.max_requests,
            window_ms: self.window_ms,
            buckets: self.buckets.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for RateLimitGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitGuard")
            .field("max_requests", &self.max_requests)
            .field("window_ms", &self.window_ms)
            .finish()
    }
}

#[async_trait]
impl<T> Transition<T, T> for RateLimitGuard<T>
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

        let mut buckets = self.buckets.lock().await;
        let now = Instant::now();
        let rate = self.max_requests as f64 / self.window_ms as f64 * 1000.0;

        let bucket = buckets.entry(client_id).or_insert(RateBucket {
            tokens: self.max_requests as f64,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed_ms = now.duration_since(bucket.last_refill).as_millis() as f64;
        bucket.tokens = (bucket.tokens + elapsed_ms * rate / 1000.0).min(self.max_requests as f64);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Outcome::next(input)
        } else {
            let retry_after = ((1.0 - bucket.tokens) / rate * 1000.0) as u64;
            Outcome::fault(format!(
                "Rate limit exceeded. Retry after {}ms",
                retry_after
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// SecurityHeadersGuard
// ---------------------------------------------------------------------------

/// Security policy configuration for HTTP response headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicy {
    pub x_frame_options: String,
    pub x_content_type_options: String,
    pub strict_transport_security: String,
    pub content_security_policy: Option<String>,
    pub x_xss_protection: String,
    pub referrer_policy: String,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            x_frame_options: "DENY".to_string(),
            x_content_type_options: "nosniff".to_string(),
            strict_transport_security: "max-age=31536000; includeSubDomains".to_string(),
            content_security_policy: None,
            x_xss_protection: "1; mode=block".to_string(),
            referrer_policy: "strict-origin-when-cross-origin".to_string(),
        }
    }
}

impl SecurityPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_csp(mut self, csp: impl Into<String>) -> Self {
        self.content_security_policy = Some(csp.into());
        self
    }
}

/// Security headers stored in the Bus for the HTTP layer to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityHeaders(pub SecurityPolicy);

/// Security headers guard — injects standard security headers into the Bus.
#[derive(Debug, Clone)]
pub struct SecurityHeadersGuard<T> {
    policy: SecurityPolicy,
    _marker: PhantomData<T>,
}

impl<T> SecurityHeadersGuard<T> {
    pub fn new(policy: SecurityPolicy) -> Self {
        Self {
            policy,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for SecurityHeadersGuard<T>
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
        bus.insert(SecurityHeaders(self.policy.clone()));
        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// IpFilterGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the client IP address.
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

/// IP filter mode.
#[derive(Debug, Clone)]
pub enum IpFilterMode {
    /// Only allow IPs in the set.
    AllowList(HashSet<String>),
    /// Block IPs in the set.
    DenyList(HashSet<String>),
}

/// IP filter guard — allows or denies requests based on client IP.
///
/// Reads `ClientIp` from the Bus.
#[derive(Debug, Clone)]
pub struct IpFilterGuard<T> {
    mode: IpFilterMode,
    _marker: PhantomData<T>,
}

impl<T> IpFilterGuard<T> {
    pub fn allow_list(ips: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            mode: IpFilterMode::AllowList(ips.into_iter().map(|s| s.into()).collect()),
            _marker: PhantomData,
        }
    }

    pub fn deny_list(ips: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            mode: IpFilterMode::DenyList(ips.into_iter().map(|s| s.into()).collect()),
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<T> Transition<T, T> for IpFilterGuard<T>
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
        let client_ip = bus
            .read::<ClientIp>()
            .map(|ip| ip.0.clone())
            .unwrap_or_default();

        match &self.mode {
            IpFilterMode::AllowList(allowed) => {
                if allowed.contains(&client_ip) {
                    Outcome::next(input)
                } else {
                    Outcome::fault(format!("IP '{}' not in allow list", client_ip))
                }
            }
            IpFilterMode::DenyList(denied) => {
                if denied.contains(&client_ip) {
                    Outcome::fault(format!("IP '{}' is denied", client_ip))
                } else {
                    Outcome::next(input)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cors_guard_allows_wildcard() {
        let guard = CorsGuard::<String>::new(CorsConfig::default());
        let mut bus = Bus::new();
        bus.insert(RequestOrigin("https://example.com".into()));
        let result = guard.run("hello".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        assert!(bus.read::<CorsHeaders>().is_some());
    }

    #[tokio::test]
    async fn cors_guard_rejects_disallowed_origin() {
        let config = CorsConfig {
            allowed_origins: vec!["https://trusted.com".into()],
            ..Default::default()
        };
        let guard = CorsGuard::<String>::new(config);
        let mut bus = Bus::new();
        bus.insert(RequestOrigin("https://evil.com".into()));
        let result = guard.run("hello".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn rate_limit_allows_within_budget() {
        let guard = RateLimitGuard::<String>::new(10, 1000);
        let mut bus = Bus::new();
        bus.insert(ClientIdentity("user1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn rate_limit_exhausts_budget() {
        let guard = RateLimitGuard::<String>::new(2, 60000);
        let mut bus = Bus::new();
        bus.insert(ClientIdentity("user1".into()));

        // Use up the budget
        let _ = guard.run("1".into(), &(), &mut bus).await;
        let _ = guard.run("2".into(), &(), &mut bus).await;
        let result = guard.run("3".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn security_headers_injects_policy() {
        let guard = SecurityHeadersGuard::<String>::new(SecurityPolicy::default());
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let headers = bus.read::<SecurityHeaders>().unwrap();
        assert_eq!(headers.0.x_frame_options, "DENY");
    }

    #[tokio::test]
    async fn ip_filter_allow_list_permits() {
        let guard = IpFilterGuard::<String>::allow_list(["10.0.0.1"]);
        let mut bus = Bus::new();
        bus.insert(ClientIp("10.0.0.1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn ip_filter_allow_list_denies() {
        let guard = IpFilterGuard::<String>::allow_list(["10.0.0.1"]);
        let mut bus = Bus::new();
        bus.insert(ClientIp("192.168.1.1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn ip_filter_deny_list_blocks() {
        let guard = IpFilterGuard::<String>::deny_list(["10.0.0.1"]);
        let mut bus = Bus::new();
        bus.insert(ClientIp("10.0.0.1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn ip_filter_deny_list_allows() {
        let guard = IpFilterGuard::<String>::deny_list(["10.0.0.1"]);
        let mut bus = Bus::new();
        bus.insert(ClientIp("192.168.1.1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    // --- AccessLogGuard tests ---

    #[tokio::test]
    async fn access_log_guard_passes_input_through() {
        let guard = AccessLogGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "GET".into(),
            path: "/users".into(),
        });
        let result = guard.run("payload".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(ref v) if v == "payload"));
    }

    #[tokio::test]
    async fn access_log_guard_writes_entry_to_bus() {
        let guard = AccessLogGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "POST".into(),
            path: "/api/orders".into(),
        });
        let _result = guard.run("ok".into(), &(), &mut bus).await;
        let entry = bus.read::<AccessLogEntry>().expect("entry should be in bus");
        assert_eq!(entry.method, "POST");
        assert_eq!(entry.path, "/api/orders");
    }

    #[tokio::test]
    async fn access_log_guard_redacts_paths() {
        let guard = AccessLogGuard::<String>::new().redact_paths(vec!["/auth/login".into()]);
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "POST".into(),
            path: "/auth/login".into(),
        });
        let _result = guard.run("ok".into(), &(), &mut bus).await;
        let entry = bus.read::<AccessLogEntry>().expect("entry should be in bus");
        assert_eq!(entry.path, "[redacted]");
    }

    #[tokio::test]
    async fn access_log_guard_works_without_request_in_bus() {
        let guard = AccessLogGuard::<String>::new();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let entry = bus.read::<AccessLogEntry>().expect("entry should be in bus");
        assert_eq!(entry.method, "");
        assert_eq!(entry.path, "");
    }

    #[tokio::test]
    async fn access_log_guard_default_works() {
        let guard = AccessLogGuard::<String>::default();
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "DELETE".into(),
            path: "/api/v1/users/42".into(),
        });
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn access_log_guard_entry_has_timestamp() {
        let guard = AccessLogGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "GET".into(),
            path: "/".into(),
        });
        let _result = guard.run("ok".into(), &(), &mut bus).await;
        let entry = bus.read::<AccessLogEntry>().unwrap();
        // Timestamp should be non-zero (milliseconds since epoch)
        assert!(entry.timestamp_ms > 1_700_000_000_000);
    }

    #[tokio::test]
    async fn access_log_guard_works_with_integer_type() {
        let guard = AccessLogGuard::<i32>::new();
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "PUT".into(),
            path: "/count".into(),
        });
        let result = guard.run(42, &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(42)));
    }

    #[tokio::test]
    async fn access_log_guard_non_redacted_path_preserved() {
        let guard = AccessLogGuard::<String>::new()
            .redact_paths(vec!["/auth/login".into()]);
        let mut bus = Bus::new();
        bus.insert(AccessLogRequest {
            method: "GET".into(),
            path: "/api/public".into(),
        });
        let _result = guard.run("ok".into(), &(), &mut bus).await;
        let entry = bus.read::<AccessLogEntry>().unwrap();
        assert_eq!(entry.path, "/api/public");
    }
}

// ---------------------------------------------------------------------------
// AccessLogGuard
// ---------------------------------------------------------------------------

/// Request metadata injected into the Bus before `AccessLogGuard` runs.
///
/// Typically set by an HTTP extractor or middleware before the guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessLogRequest {
    pub method: String,
    pub path: String,
}

/// Access log entry written to the Bus by `AccessLogGuard`.
///
/// Downstream nodes can read this to inspect what was logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessLogEntry {
    pub method: String,
    pub path: String,
    pub timestamp_ms: u64,
}

/// HTTP access log guard — logs request metadata and writes an [`AccessLogEntry`]
/// to the Bus.
///
/// This is a **pass-through** guard: it always returns `Outcome::next(input)`.
/// It never faults — if no [`AccessLogRequest`] is in the Bus, it logs an empty
/// entry.
///
/// # Example
///
/// ```ignore
/// Axon::new("api")
///     .then(AccessLogGuard::new()
///         .redact_paths(vec!["/auth/login".into()]))
///     .then(CorsGuard::default())
///     .then(business_logic)
/// ```
#[derive(Debug, Clone)]
pub struct AccessLogGuard<T> {
    redact_paths: Vec<String>,
    _marker: PhantomData<T>,
}

impl<T> AccessLogGuard<T> {
    /// Create a new `AccessLogGuard` with default settings.
    pub fn new() -> Self {
        Self {
            redact_paths: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Paths whose entries will have the path replaced with `"[redacted]"`.
    ///
    /// Use this for sensitive endpoints (e.g., login, token refresh) where
    /// logging the path itself might leak information.
    pub fn redact_paths(mut self, paths: Vec<String>) -> Self {
        self.redact_paths = paths;
        self
    }
}

impl<T> Default for AccessLogGuard<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T> Transition<T, T> for AccessLogGuard<T>
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
        let req = bus.read::<AccessLogRequest>().cloned();
        let (method, raw_path) = match &req {
            Some(r) => (r.method.clone(), r.path.clone()),
            None => (String::new(), String::new()),
        };

        let display_path = if self.redact_paths.iter().any(|p| p == &raw_path) {
            "[redacted]".to_string()
        } else {
            raw_path
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        tracing::info!(method = %method, path = %display_path, "access");

        bus.insert(AccessLogEntry {
            method,
            path: display_path,
            timestamp_ms: now_ms,
        });

        Outcome::next(input)
    }
}
