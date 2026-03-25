//! # ranvier-guard — HTTP Security/Policy Guard Nodes
//!
//! Guard nodes are typed Transition nodes that enforce security and policy
//! constraints as visible, traceable pipeline steps — replacing hidden Tower
//! middleware layers.
//!
//! Each Guard reads context from the Bus (e.g., request headers, client IP)
//! and either passes the input through unchanged or returns a Fault.
//!
//! ## Available Guards
//!
//! | Guard | Purpose | Bus Read | Bus Write |
//! |-------|---------|----------|-----------|
//! | [`CorsGuard`] | Origin validation + CORS headers | `RequestOrigin` | `CorsHeaders` |
//! | [`RateLimitGuard`] | Per-client token-bucket rate limiting | `ClientIdentity` | — |
//! | [`SecurityHeadersGuard`] | Standard security response headers | — | `SecurityHeaders` |
//! | [`IpFilterGuard`] | Allow/deny-list IP filtering | `ClientIp` | — |
//! | [`AccessLogGuard`] | Structured access logging | `AccessLogRequest` | `AccessLogEntry` |
//!
//! ## Example
//!
//! ```rust,ignore
//! use ranvier_guard::*;
//!
//! Axon::simple::<String>("api")
//!     .then(AccessLogGuard::new())
//!     .then(CorsGuard::new(CorsConfig::default()))
//!     .then(SecurityHeadersGuard::new(SecurityPolicy::default()))
//!     .then(business_logic)
//! ```

use async_trait::async_trait;
use ranvier_core::iam::{enforce_policy, IamIdentity, IamPolicy};
use ranvier_core::{bus::Bus, outcome::Outcome, transition::Transition};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Instant;
use subtle::ConstantTimeEq;
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

    /// Create a fully permissive CORS guard for development and testing.
    ///
    /// Allows all origins (`*`), all standard HTTP methods, and common headers.
    /// A `tracing::warn` is emitted to remind that this should not be used in production.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Ranvier::http()
    ///     .guard(CorsGuard::<()>::permissive())
    /// ```
    pub fn permissive() -> Self {
        tracing::warn!("CorsGuard::permissive() — all origins allowed; do not use in production");
        Self {
            config: CorsConfig {
                allowed_origins: vec!["*".to_string()],
                allowed_methods: vec![
                    "GET".into(), "POST".into(), "PUT".into(), "DELETE".into(),
                    "PATCH".into(), "OPTIONS".into(), "HEAD".into(),
                ],
                allowed_headers: vec![
                    "Content-Type".into(), "Authorization".into(), "Accept".into(),
                    "Origin".into(), "X-Requested-With".into(),
                ],
                max_age_seconds: 86400,
                allow_credentials: false,
            },
            _marker: PhantomData,
        }
    }

    /// Returns a reference to the CORS configuration.
    pub fn cors_config(&self) -> &CorsConfig {
        &self.config
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
///
/// Stale buckets are automatically pruned when `bucket_ttl` is set.
/// The TTL check runs lazily on each request (no background task required).
pub struct RateLimitGuard<T> {
    max_requests: u64,
    window_ms: u64,
    buckets: Arc<Mutex<std::collections::HashMap<String, RateBucket>>>,
    /// If > 0, buckets idle longer than this (in ms) are removed on next access.
    bucket_ttl_ms: u64,
    _marker: PhantomData<T>,
}

impl<T> RateLimitGuard<T> {
    pub fn new(max_requests: u64, window_ms: u64) -> Self {
        Self {
            max_requests,
            window_ms,
            buckets: Arc::new(Mutex::new(std::collections::HashMap::new())),
            bucket_ttl_ms: 0,
            _marker: PhantomData,
        }
    }

    /// Set a TTL for idle buckets. Buckets not accessed within this duration
    /// are lazily pruned on subsequent requests.
    ///
    /// Default: no TTL (buckets persist forever).
    pub fn with_bucket_ttl(mut self, ttl: std::time::Duration) -> Self {
        self.bucket_ttl_ms = ttl.as_millis() as u64;
        self
    }

    /// Returns the maximum requests per window.
    pub fn max_requests(&self) -> u64 {
        self.max_requests
    }

    /// Returns the window duration in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Returns the configured bucket TTL in milliseconds (0 = disabled).
    pub fn bucket_ttl_ms(&self) -> u64 {
        self.bucket_ttl_ms
    }
}

impl<T> Clone for RateLimitGuard<T> {
    fn clone(&self) -> Self {
        Self {
            max_requests: self.max_requests,
            window_ms: self.window_ms,
            buckets: self.buckets.clone(),
            bucket_ttl_ms: self.bucket_ttl_ms,
            _marker: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for RateLimitGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitGuard")
            .field("max_requests", &self.max_requests)
            .field("window_ms", &self.window_ms)
            .field("bucket_ttl_ms", &self.bucket_ttl_ms)
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

        // Lazy prune: remove stale buckets that haven't been accessed within the TTL
        if self.bucket_ttl_ms > 0 {
            let ttl = std::time::Duration::from_millis(self.bucket_ttl_ms);
            buckets.retain(|_, b| now.duration_since(b.last_refill) < ttl);
        }

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

    /// Returns a reference to the security policy.
    pub fn policy(&self) -> &SecurityPolicy {
        &self.policy
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

/// A set of trusted proxy IPs for safe X-Forwarded-For extraction.
///
/// When the direct connection comes from a trusted proxy, the rightmost
/// non-trusted IP in the X-Forwarded-For chain is used as the client IP.
/// If the direct connection is NOT from a trusted proxy, X-Forwarded-For
/// is ignored and the direct connection IP is used instead.
///
/// ## Example
///
/// ```rust,ignore
/// use ranvier_guard::TrustedProxies;
///
/// let proxies = TrustedProxies::new(["10.0.0.1", "10.0.0.2"]);
///
/// // Direct IP from trusted proxy, XFF has client chain
/// let ip = proxies.extract("203.0.113.50, 10.0.0.1", "10.0.0.2");
/// assert_eq!(ip, "203.0.113.50");
///
/// // Direct IP is NOT a trusted proxy → ignore XFF
/// let ip = proxies.extract("spoofed-ip", "198.51.100.7");
/// assert_eq!(ip, "198.51.100.7");
/// ```
#[derive(Debug, Clone)]
pub struct TrustedProxies {
    proxies: HashSet<String>,
}

impl TrustedProxies {
    /// Create a new TrustedProxies set.
    pub fn new(ips: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            proxies: ips.into_iter().map(|s| s.into()).collect(),
        }
    }

    /// Extract the real client IP from X-Forwarded-For header and direct connection IP.
    ///
    /// If the `direct_ip` is NOT in the trusted set, XFF is ignored (anti-spoofing).
    /// Otherwise, walks the XFF chain right-to-left and returns the first non-trusted IP.
    pub fn extract(&self, xff_header: &str, direct_ip: &str) -> String {
        // If direct connection is not from a trusted proxy, don't trust XFF
        if !self.proxies.contains(direct_ip) {
            return direct_ip.to_string();
        }

        // Walk the XFF chain right-to-left, skip trusted proxies
        let parts: Vec<&str> = xff_header.split(',').map(|s| s.trim()).collect();
        for ip in parts.iter().rev() {
            if !ip.is_empty() && !self.proxies.contains(*ip) {
                return ip.to_string();
            }
        }

        // Fallback: all IPs in XFF are trusted proxies, use direct IP
        direct_ip.to_string()
    }

    /// Check if the given IP is a trusted proxy.
    pub fn is_trusted(&self, ip: &str) -> bool {
        self.proxies.contains(ip)
    }
}

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

    /// Clone the guard configuration as `IpFilterGuard<()>` for type-erased execution.
    pub fn clone_as_unit(&self) -> IpFilterGuard<()> {
        IpFilterGuard {
            mode: self.mode.clone(),
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

    /// Clone the guard configuration as `AccessLogGuard<()>` for type-erased execution.
    pub fn clone_as_unit(&self) -> AccessLogGuard<()> {
        AccessLogGuard {
            redact_paths: self.redact_paths.clone(),
            _marker: PhantomData,
        }
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

// ---------------------------------------------------------------------------
// CompressionGuard
// ---------------------------------------------------------------------------

/// Supported compression encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionEncoding {
    Gzip,
    Brotli,
    Zstd,
    Identity,
}

impl CompressionEncoding {
    /// HTTP `Content-Encoding` header value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Brotli => "br",
            Self::Zstd => "zstd",
            Self::Identity => "identity",
        }
    }
}

/// Bus-injectable type representing the client's `Accept-Encoding` header.
#[derive(Debug, Clone)]
pub struct AcceptEncoding(pub String);

/// Compression configuration written to the Bus after encoding negotiation.
///
/// The HTTP response layer reads this to decide whether and how to compress
/// the response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    pub encoding: CompressionEncoding,
    pub min_body_size: usize,
}

/// Compression guard — negotiates response encoding from `Accept-Encoding`.
///
/// Reads [`AcceptEncoding`] from the Bus and selects the best encoding
/// based on the configured preference order. Writes [`CompressionConfig`]
/// to the Bus for the HTTP layer to apply.
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .guard(CompressionGuard::new().prefer_brotli())
///     .get("/api/data", circuit)
/// ```
#[derive(Debug, Clone)]
pub struct CompressionGuard<T> {
    preferred: Vec<CompressionEncoding>,
    min_body_size: usize,
    _marker: PhantomData<T>,
}

impl<T> CompressionGuard<T> {
    /// Create with default preference order: gzip > identity.
    pub fn new() -> Self {
        Self {
            preferred: vec![CompressionEncoding::Gzip, CompressionEncoding::Identity],
            min_body_size: 256,
            _marker: PhantomData,
        }
    }

    /// Set preference order to brotli > gzip > identity.
    pub fn prefer_brotli(mut self) -> Self {
        self.preferred = vec![
            CompressionEncoding::Brotli,
            CompressionEncoding::Gzip,
            CompressionEncoding::Identity,
        ];
        self
    }

    /// Set minimum body size for compression (default: 256 bytes).
    pub fn with_min_body_size(mut self, size: usize) -> Self {
        self.min_body_size = size;
        self
    }

    /// Returns the minimum body size threshold.
    pub fn min_body_size(&self) -> usize {
        self.min_body_size
    }

    /// Returns the preference order.
    pub fn preferred_encodings(&self) -> &[CompressionEncoding] {
        &self.preferred
    }
}

impl<T> Default for CompressionGuard<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `Accept-Encoding` header value into a set of supported encodings.
fn parse_accept_encoding(header: &str) -> HashSet<String> {
    header
        .split(',')
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[async_trait]
impl<T> Transition<T, T> for CompressionGuard<T>
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
        let accepted = bus
            .read::<AcceptEncoding>()
            .map(|ae| parse_accept_encoding(&ae.0))
            .unwrap_or_default();

        // Negotiate: pick first preferred encoding the client accepts
        let selected = if accepted.is_empty() || accepted.contains("*") {
            self.preferred.first().copied().unwrap_or(CompressionEncoding::Identity)
        } else {
            self.preferred
                .iter()
                .find(|enc| accepted.contains(enc.as_str()))
                .copied()
                .unwrap_or(CompressionEncoding::Identity)
        };

        bus.insert(CompressionConfig {
            encoding: selected,
            min_body_size: self.min_body_size,
        });

        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// RequestSizeLimitGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the request's `Content-Length` header value.
#[derive(Debug, Clone)]
pub struct ContentLength(pub u64);

/// Request body size limit guard — rejects requests exceeding the configured
/// maximum `Content-Length`.
///
/// Reads [`ContentLength`] from the Bus. If the value exceeds the limit,
/// returns a Fault with "413 Payload Too Large".
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .guard(RequestSizeLimitGuard::max_2mb())
///     .post("/api/upload", upload_circuit)
/// ```
#[derive(Debug, Clone)]
pub struct RequestSizeLimitGuard<T> {
    max_bytes: u64,
    _marker: PhantomData<T>,
}

impl<T> RequestSizeLimitGuard<T> {
    /// Create with a custom byte limit.
    pub fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            _marker: PhantomData,
        }
    }

    /// 2 MB limit.
    pub fn max_2mb() -> Self {
        Self::new(2 * 1024 * 1024)
    }

    /// 10 MB limit.
    pub fn max_10mb() -> Self {
        Self::new(10 * 1024 * 1024)
    }

    /// Returns the configured maximum bytes.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

#[async_trait]
impl<T> Transition<T, T> for RequestSizeLimitGuard<T>
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
        if let Some(len) = bus.read::<ContentLength>() {
            if len.0 > self.max_bytes {
                return Outcome::fault(format!(
                    "413 Payload Too Large: {} bytes exceeds limit of {} bytes",
                    len.0, self.max_bytes
                ));
            }
        }
        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// RequestIdGuard
// ---------------------------------------------------------------------------

/// Bus type representing a unique request identifier.
///
/// Propagated from the `X-Request-Id` header or generated as UUID v4.
/// The HTTP response layer reflects this back in the `X-Request-Id` response header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestId(pub String);

/// Request ID guard — ensures every request has a unique identifier.
///
/// If `RequestId` is already in the Bus (from a client-provided `X-Request-Id`
/// header), it is preserved. Otherwise a UUID v4 is generated.
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .guard(RequestIdGuard::new())
///     .get("/api/data", circuit)
/// ```
#[derive(Debug, Clone)]
pub struct RequestIdGuard<T> {
    _marker: PhantomData<T>,
}

impl<T> RequestIdGuard<T> {
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for RequestIdGuard<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T> Transition<T, T> for RequestIdGuard<T>
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
        // Generate a UUID v4 if no RequestId was injected by the HTTP layer
        if bus.read::<RequestId>().is_none() {
            bus.insert(RequestId(uuid::Uuid::new_v4().to_string()));
        }

        // Integrate with tracing: record request_id on current span
        if let Some(rid) = bus.read::<RequestId>() {
            tracing::debug!(request_id = %rid.0, "request id assigned");
        }

        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// AuthGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the raw `Authorization` header value.
#[derive(Debug, Clone)]
pub struct AuthorizationHeader(pub String);

/// Authentication strategy for [`AuthGuard`].
pub enum AuthStrategy {
    /// Bearer token authentication.
    ///
    /// Compares the request's `Authorization: Bearer <token>` against a set
    /// of valid tokens using constant-time comparison to prevent timing attacks.
    Bearer {
        tokens: Vec<String>,
    },

    /// API key authentication from a custom header.
    ///
    /// Validates the value of `header_name` against a set of valid keys
    /// using constant-time comparison.
    ApiKey {
        header_name: String,
        valid_keys: Vec<String>,
    },

    /// Custom authentication via a validator function.
    ///
    /// The function receives the raw `Authorization` header value and returns
    /// either a verified [`IamIdentity`] or an error message.
    Custom {
        validator: Arc<dyn Fn(&str) -> Result<IamIdentity, String> + Send + Sync + 'static>,
    },
}

impl Clone for AuthStrategy {
    fn clone(&self) -> Self {
        match self {
            Self::Bearer { tokens } => Self::Bearer {
                tokens: tokens.clone(),
            },
            Self::ApiKey {
                header_name,
                valid_keys,
            } => Self::ApiKey {
                header_name: header_name.clone(),
                valid_keys: valid_keys.clone(),
            },
            Self::Custom { validator } => Self::Custom {
                validator: validator.clone(),
            },
        }
    }
}

impl std::fmt::Debug for AuthStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer { tokens } => f
                .debug_struct("Bearer")
                .field("token_count", &tokens.len())
                .finish(),
            Self::ApiKey { header_name, valid_keys } => f
                .debug_struct("ApiKey")
                .field("header_name", header_name)
                .field("key_count", &valid_keys.len())
                .finish(),
            Self::Custom { .. } => f.debug_struct("Custom").finish(),
        }
    }
}

/// Authentication guard — validates credentials and injects [`IamIdentity`]
/// into the Bus.
///
/// Supports Bearer token, API key, and custom authentication strategies.
/// Uses constant-time comparison (`subtle::ConstantTimeEq`) for Bearer and
/// API key validation to prevent timing attacks.
///
/// # Examples
///
/// ```rust,ignore
/// // Bearer token auth
/// Ranvier::http()
///     .guard(AuthGuard::bearer(vec!["secret-token".into()]))
///     .get("/api/protected", circuit)
///
/// // With role requirement
/// Ranvier::http()
///     .guard(AuthGuard::bearer(vec!["admin-token".into()])
///         .with_policy(IamPolicy::RequireRole("admin".into())))
///     .get("/api/admin", circuit)
/// ```
pub struct AuthGuard<T> {
    strategy: AuthStrategy,
    policy: IamPolicy,
    _marker: PhantomData<T>,
}

impl<T> AuthGuard<T> {
    /// Create with a specific strategy and no policy enforcement.
    pub fn new(strategy: AuthStrategy) -> Self {
        Self {
            strategy,
            policy: IamPolicy::None,
            _marker: PhantomData,
        }
    }

    /// Bearer token authentication.
    pub fn bearer(tokens: Vec<String>) -> Self {
        Self::new(AuthStrategy::Bearer { tokens })
    }

    /// API key authentication from a custom header.
    pub fn api_key(header_name: impl Into<String>, valid_keys: Vec<String>) -> Self {
        Self::new(AuthStrategy::ApiKey {
            header_name: header_name.into(),
            valid_keys,
        })
    }

    /// Custom validator function.
    pub fn custom(
        validator: impl Fn(&str) -> Result<IamIdentity, String> + Send + Sync + 'static,
    ) -> Self {
        Self::new(AuthStrategy::Custom {
            validator: Arc::new(validator),
        })
    }

    /// Set the IAM policy to enforce after successful authentication.
    pub fn with_policy(mut self, policy: IamPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Returns the authentication strategy.
    pub fn strategy(&self) -> &AuthStrategy {
        &self.strategy
    }

    /// Returns the IAM policy.
    pub fn iam_policy(&self) -> &IamPolicy {
        &self.policy
    }
}

impl<T> Clone for AuthGuard<T> {
    fn clone(&self) -> Self {
        Self {
            strategy: self.strategy.clone(),
            policy: self.policy.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for AuthGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthGuard")
            .field("strategy", &self.strategy)
            .field("policy", &self.policy)
            .finish()
    }
}

/// Constant-time comparison of two byte slices.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.ct_eq(b).into()
}

#[async_trait]
impl<T> Transition<T, T> for AuthGuard<T>
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
        let auth_value = bus.read::<AuthorizationHeader>().map(|h| h.0.clone());

        let identity = match &self.strategy {
            AuthStrategy::Bearer { tokens } => {
                let Some(auth) = auth_value else {
                    return Outcome::fault(
                        "401 Unauthorized: missing Authorization header".to_string(),
                    );
                };
                let Some(token) = auth.strip_prefix("Bearer ") else {
                    return Outcome::fault(
                        "401 Unauthorized: expected Bearer scheme".to_string(),
                    );
                };
                let token = token.trim();
                let matched = tokens
                    .iter()
                    .any(|valid| ct_eq(token.as_bytes(), valid.as_bytes()));
                if !matched {
                    return Outcome::fault(
                        "401 Unauthorized: invalid bearer token".to_string(),
                    );
                }
                IamIdentity::new("bearer-authenticated")
            }
            AuthStrategy::ApiKey { valid_keys, .. } => {
                let Some(key) = auth_value else {
                    return Outcome::fault("401 Unauthorized: missing API key".to_string());
                };
                let matched = valid_keys
                    .iter()
                    .any(|valid| ct_eq(key.as_bytes(), valid.as_bytes()));
                if !matched {
                    return Outcome::fault("401 Unauthorized: invalid API key".to_string());
                }
                IamIdentity::new("apikey-authenticated")
            }
            AuthStrategy::Custom { validator } => {
                let raw = auth_value.unwrap_or_default();
                match validator(&raw) {
                    Ok(identity) => identity,
                    Err(msg) => {
                        return Outcome::fault(format!("401 Unauthorized: {}", msg));
                    }
                }
            }
        };

        // Enforce IAM policy
        if let Err(e) = enforce_policy(&self.policy, &identity) {
            return Outcome::fault(format!("403 Forbidden: {}", e));
        }

        bus.insert(identity);
        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// ContentTypeGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the request's `Content-Type` header value.
#[derive(Debug, Clone)]
pub struct RequestContentType(pub String);

/// Content-Type validation guard — rejects requests with unsupported media types.
///
/// Reads [`RequestContentType`] from the Bus. If the content type does not
/// match any of the allowed types, returns a Fault with "415 Unsupported Media Type".
///
/// Useful as a per-route guard: apply to POST/PUT/PATCH endpoints while
/// leaving GET/DELETE endpoints unrestricted.
///
/// # Example
///
/// ```rust,ignore
/// Ranvier::http()
///     .post_with_guards("/api/data", circuit, guards![
///         ContentTypeGuard::json(),
///     ])
/// ```
#[derive(Debug, Clone)]
pub struct ContentTypeGuard<T> {
    allowed_types: Vec<String>,
    _marker: PhantomData<T>,
}

impl<T> ContentTypeGuard<T> {
    /// Create with specific allowed content types.
    pub fn new(allowed_types: Vec<String>) -> Self {
        Self {
            allowed_types,
            _marker: PhantomData,
        }
    }

    /// Accept only `application/json`.
    pub fn json() -> Self {
        Self::new(vec!["application/json".into()])
    }

    /// Accept only `application/x-www-form-urlencoded`.
    pub fn form() -> Self {
        Self::new(vec!["application/x-www-form-urlencoded".into()])
    }

    /// Accept specific content types.
    pub fn accept(types: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new(types.into_iter().map(|t| t.into()).collect())
    }

    /// Returns the allowed content types.
    pub fn allowed_types(&self) -> &[String] {
        &self.allowed_types
    }
}

#[async_trait]
impl<T> Transition<T, T> for ContentTypeGuard<T>
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
        let content_type = bus.read::<RequestContentType>().map(|ct| ct.0.clone());

        // If no Content-Type header, allow (GET/DELETE may not have body)
        let Some(ct) = content_type else {
            return Outcome::next(input);
        };

        // Compare the media type portion (before any ;charset=... parameters)
        let media_type = ct.split(';').next().unwrap_or("").trim().to_lowercase();
        let matched = self
            .allowed_types
            .iter()
            .any(|allowed| allowed.to_lowercase() == media_type);

        if matched {
            Outcome::next(input)
        } else {
            Outcome::fault(format!(
                "415 Unsupported Media Type: expected one of [{}], got '{}'",
                self.allowed_types.join(", "),
                media_type,
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// TimeoutGuard
// ---------------------------------------------------------------------------

/// Deadline for the current request pipeline.
///
/// Written to the Bus by [`TimeoutGuard`]. The HTTP ingress layer reads this
/// to enforce the deadline by wrapping circuit execution with
/// `tokio::time::timeout()`.
///
/// Complements `Axon::then_with_timeout()`: TimeoutGuard sets the global
/// pipeline deadline, while `then_with_timeout()` adds per-node timeouts.
#[derive(Debug, Clone)]
pub struct TimeoutDeadline {
    created_at: std::time::Instant,
    timeout: std::time::Duration,
}

impl TimeoutDeadline {
    /// Create a new deadline starting from now.
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            created_at: std::time::Instant::now(),
            timeout,
        }
    }

    /// Returns the remaining time until the deadline.
    pub fn remaining(&self) -> std::time::Duration {
        self.timeout.saturating_sub(self.created_at.elapsed())
    }

    /// Returns true if the deadline has passed.
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.timeout
    }

    /// Returns the configured timeout duration.
    pub fn duration(&self) -> std::time::Duration {
        self.timeout
    }
}

/// Pipeline timeout guard — sets a [`TimeoutDeadline`] in the Bus.
///
/// This is a **pass-through** guard that writes the deadline. The HTTP
/// ingress layer enforces it by wrapping circuit execution with
/// `tokio::time::timeout()`.
///
/// # Relationship with `Axon::then_with_timeout()`
///
/// - `TimeoutGuard`: outer boundary — limits total pipeline duration
/// - `Axon::then_with_timeout()`: inner granularity — limits a single node
///
/// # Example
///
/// ```rust,ignore
/// use std::time::Duration;
///
/// Ranvier::http()
///     .guard(TimeoutGuard::new(Duration::from_secs(30)))
///     .post("/api/slow", slow_circuit)
/// ```
#[derive(Debug, Clone)]
pub struct TimeoutGuard<T> {
    timeout: std::time::Duration,
    _marker: PhantomData<T>,
}

impl<T> TimeoutGuard<T> {
    /// Create with a specific timeout.
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            timeout,
            _marker: PhantomData,
        }
    }

    /// 5-second timeout.
    pub fn secs_5() -> Self {
        Self::new(std::time::Duration::from_secs(5))
    }

    /// 30-second timeout.
    pub fn secs_30() -> Self {
        Self::new(std::time::Duration::from_secs(30))
    }

    /// 60-second timeout.
    pub fn secs_60() -> Self {
        Self::new(std::time::Duration::from_secs(60))
    }

    /// Returns the configured timeout.
    pub fn timeout(&self) -> std::time::Duration {
        self.timeout
    }
}

#[async_trait]
impl<T> Transition<T, T> for TimeoutGuard<T>
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
        bus.insert(TimeoutDeadline::new(self.timeout));
        Outcome::next(input)
    }
}

// ---------------------------------------------------------------------------
// IdempotencyGuard
// ---------------------------------------------------------------------------

/// Bus-injectable type representing the `Idempotency-Key` header value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyKey(pub String);

/// Cached response from a previous idempotent request.
///
/// When found in the Bus after guard execution, the HTTP ingress skips
/// circuit execution and returns the cached response directly with an
/// `Idempotency-Replayed: true` header.
#[derive(Debug, Clone)]
pub struct IdempotencyCachedResponse {
    pub body: Vec<u8>,
}

/// TTL-based in-memory cache entry for idempotency.
struct IdempotencyCacheEntry {
    body: Vec<u8>,
    expires_at: std::time::Instant,
}

/// Shared TTL-based in-memory cache for idempotency key tracking.
#[derive(Clone)]
pub struct IdempotencyCache {
    inner: Arc<std::sync::Mutex<std::collections::HashMap<String, IdempotencyCacheEntry>>>,
    ttl: std::time::Duration,
}

impl IdempotencyCache {
    /// Create a new cache with the given TTL.
    pub fn new(ttl: std::time::Duration) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            ttl,
        }
    }

    /// Look up a cached response body by key. Returns `None` if not found or expired.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut cache = self.inner.lock().ok()?;
        let now = std::time::Instant::now();
        if let Some(entry) = cache.get(key) {
            if entry.expires_at > now {
                return Some(entry.body.clone());
            }
            cache.remove(key);
        }
        None
    }

    /// Insert a response body into the cache.
    pub fn insert(&self, key: String, body: Vec<u8>) {
        if let Ok(mut cache) = self.inner.lock() {
            let now = std::time::Instant::now();
            // Lazy cleanup: remove a few expired entries on insert
            let expired: Vec<String> = cache
                .iter()
                .filter(|(_, e)| e.expires_at <= now)
                .take(5)
                .map(|(k, _)| k.clone())
                .collect();
            for k in expired {
                cache.remove(&k);
            }
            cache.insert(
                key,
                IdempotencyCacheEntry {
                    body,
                    expires_at: now + self.ttl,
                },
            );
        }
    }

    /// Returns the configured TTL.
    pub fn ttl(&self) -> std::time::Duration {
        self.ttl
    }
}

impl std::fmt::Debug for IdempotencyCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdempotencyCache")
            .field("ttl", &self.ttl)
            .finish()
    }
}

/// Idempotency guard — prevents duplicate request processing via an
/// in-memory TTL cache.
///
/// Reads [`IdempotencyKey`] from the Bus (extracted from the `Idempotency-Key`
/// HTTP header). On cache hit, writes [`IdempotencyCachedResponse`] to the
/// Bus to signal the ingress layer to skip circuit execution.
///
/// On cache miss, the HTTP ingress layer caches the response body after
/// circuit execution via `ResponseBodyTransformFn`.
///
/// # Example
///
/// ```rust,ignore
/// use std::time::Duration;
///
/// Ranvier::http()
///     .post_with_guards("/api/orders", order_circuit, guards![
///         ContentTypeGuard::json(),
///         IdempotencyGuard::new(Duration::from_secs(300)),
///     ])
/// ```
pub struct IdempotencyGuard<T> {
    cache: IdempotencyCache,
    _marker: PhantomData<T>,
}

impl<T> IdempotencyGuard<T> {
    /// Create with a specific TTL for cached entries.
    pub fn new(ttl: std::time::Duration) -> Self {
        Self {
            cache: IdempotencyCache::new(ttl),
            _marker: PhantomData,
        }
    }

    /// 5-minute TTL (default for most APIs).
    pub fn ttl_5min() -> Self {
        Self::new(std::time::Duration::from_secs(300))
    }

    /// Returns the configured TTL.
    pub fn ttl(&self) -> std::time::Duration {
        self.cache.ttl()
    }

    /// Returns a reference to the internal cache.
    pub fn cache(&self) -> &IdempotencyCache {
        &self.cache
    }

    /// Clone the guard configuration as `IdempotencyGuard<()>` for type-erased execution.
    pub fn clone_as_unit(&self) -> IdempotencyGuard<()> {
        IdempotencyGuard {
            cache: self.cache.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> Clone for IdempotencyGuard<T> {
    fn clone(&self) -> Self {
        Self {
            cache: self.cache.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for IdempotencyGuard<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdempotencyGuard")
            .field("ttl", &self.cache.ttl())
            .finish()
    }
}

#[async_trait]
impl<T> Transition<T, T> for IdempotencyGuard<T>
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
        let Some(key) = bus.read::<IdempotencyKey>().map(|k| k.0.clone()) else {
            return Outcome::next(input);
        };

        if let Some(body) = self.cache.get(&key) {
            bus.insert(IdempotencyCachedResponse { body });
            tracing::debug!(idempotency_key = %key, "idempotency cache hit");
        } else {
            tracing::debug!(idempotency_key = %key, "idempotency cache miss");
        }

        Outcome::next(input)
    }
}

// ===========================================================================
// Tier 3 Guards (feature-gated: `advanced`)
// ===========================================================================

#[cfg(feature = "advanced")]
mod advanced_guards;

#[cfg(feature = "advanced")]
pub use advanced_guards::*;

#[cfg(feature = "distributed")]
mod distributed;

#[cfg(feature = "distributed")]
pub use distributed::DistributedRateLimitGuard;

// ---------------------------------------------------------------------------
// Prelude
// ---------------------------------------------------------------------------

pub mod prelude {
    pub use crate::{
        AcceptEncoding, AccessLogEntry, AccessLogGuard, AccessLogRequest, AuthGuard,
        AuthStrategy, AuthorizationHeader, ClientIdentity, ClientIp, CompressionConfig,
        CompressionEncoding, CompressionGuard, ContentLength, ContentTypeGuard, CorsConfig,
        CorsGuard, CorsHeaders, IdempotencyCache, IdempotencyCachedResponse, IdempotencyGuard,
        IdempotencyKey, IpFilterGuard, RateLimitGuard, RequestContentType, RequestId,
        RequestIdGuard, RequestOrigin, RequestSizeLimitGuard, SecurityHeaders,
        SecurityHeadersGuard, SecurityPolicy, TimeoutDeadline, TimeoutGuard,
    };

    #[cfg(feature = "advanced")]
    pub use crate::advanced_guards::{
        ConditionalRequestGuard, DecompressionGuard, ETag, IfModifiedSince, IfNoneMatch,
        LastModified, RedirectGuard, RedirectRule, RequestBody,
    };

    #[cfg(feature = "distributed")]
    pub use crate::distributed::DistributedRateLimitGuard;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    // --- CompressionGuard tests ---

    #[tokio::test]
    async fn compression_guard_negotiates_gzip() {
        let guard = CompressionGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(AcceptEncoding("gzip, deflate".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let config = bus.read::<CompressionConfig>().unwrap();
        assert_eq!(config.encoding, CompressionEncoding::Gzip);
    }

    #[tokio::test]
    async fn compression_guard_prefer_brotli() {
        let guard = CompressionGuard::<String>::new().prefer_brotli();
        let mut bus = Bus::new();
        bus.insert(AcceptEncoding("gzip, br, zstd".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let config = bus.read::<CompressionConfig>().unwrap();
        assert_eq!(config.encoding, CompressionEncoding::Brotli);
    }

    #[tokio::test]
    async fn compression_guard_falls_back_to_identity() {
        let guard = CompressionGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(AcceptEncoding("deflate".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let config = bus.read::<CompressionConfig>().unwrap();
        assert_eq!(config.encoding, CompressionEncoding::Identity);
    }

    #[tokio::test]
    async fn compression_guard_wildcard_accept() {
        let guard = CompressionGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(AcceptEncoding("*".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let config = bus.read::<CompressionConfig>().unwrap();
        assert_eq!(config.encoding, CompressionEncoding::Gzip);
    }

    #[tokio::test]
    async fn compression_guard_min_body_size() {
        let guard = CompressionGuard::<String>::new().with_min_body_size(1024);
        let mut bus = Bus::new();
        bus.insert(AcceptEncoding("gzip".into()));
        let _ = guard.run("ok".into(), &(), &mut bus).await;
        let config = bus.read::<CompressionConfig>().unwrap();
        assert_eq!(config.min_body_size, 1024);
    }

    // --- RequestSizeLimitGuard tests ---

    #[tokio::test]
    async fn size_limit_allows_within_limit() {
        let guard = RequestSizeLimitGuard::<String>::max_2mb();
        let mut bus = Bus::new();
        bus.insert(ContentLength(1024));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn size_limit_rejects_over_limit() {
        let guard = RequestSizeLimitGuard::<String>::new(1000);
        let mut bus = Bus::new();
        bus.insert(ContentLength(2000));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("413")));
    }

    #[tokio::test]
    async fn size_limit_passes_without_content_length() {
        let guard = RequestSizeLimitGuard::<String>::new(100);
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn size_limit_convenience_constructors() {
        let guard_2mb = RequestSizeLimitGuard::<()>::max_2mb();
        assert_eq!(guard_2mb.max_bytes(), 2 * 1024 * 1024);

        let guard_10mb = RequestSizeLimitGuard::<()>::max_10mb();
        assert_eq!(guard_10mb.max_bytes(), 10 * 1024 * 1024);
    }

    // --- RequestIdGuard tests ---

    #[tokio::test]
    async fn request_id_generates_uuid() {
        let guard = RequestIdGuard::<String>::new();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let rid = bus.read::<RequestId>().expect("request id should be in bus");
        assert_eq!(rid.0.len(), 36); // UUID v4 format
    }

    #[tokio::test]
    async fn request_id_preserves_existing() {
        let guard = RequestIdGuard::<String>::new();
        let mut bus = Bus::new();
        bus.insert(RequestId("custom-id-123".into()));
        let _ = guard.run("ok".into(), &(), &mut bus).await;
        let rid = bus.read::<RequestId>().unwrap();
        assert_eq!(rid.0, "custom-id-123");
    }

    // --- AuthGuard tests ---

    #[tokio::test]
    async fn auth_bearer_success() {
        let guard = AuthGuard::<String>::bearer(vec!["secret-token".into()]);
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("Bearer secret-token".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let identity = bus.read::<IamIdentity>().expect("identity should be in bus");
        assert_eq!(identity.subject, "bearer-authenticated");
    }

    #[tokio::test]
    async fn auth_bearer_invalid_token() {
        let guard = AuthGuard::<String>::bearer(vec!["secret-token".into()]);
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("Bearer wrong-token".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("401")));
    }

    #[tokio::test]
    async fn auth_bearer_missing_header() {
        let guard = AuthGuard::<String>::bearer(vec!["token".into()]);
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("401")));
    }

    #[tokio::test]
    async fn auth_apikey_success() {
        let guard = AuthGuard::<String>::api_key("X-Api-Key", vec!["my-api-key".into()]);
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("my-api-key".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn auth_apikey_invalid() {
        let guard = AuthGuard::<String>::api_key("X-Api-Key", vec!["valid-key".into()]);
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("invalid-key".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("401")));
    }

    #[tokio::test]
    async fn auth_custom_validator() {
        let guard = AuthGuard::<String>::custom(|token| {
            if token == "Bearer magic" {
                Ok(IamIdentity::new("custom-user").with_role("admin"))
            } else {
                Err("bad token".into())
            }
        });
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("Bearer magic".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let id = bus.read::<IamIdentity>().unwrap();
        assert!(id.has_role("admin"));
    }

    #[tokio::test]
    async fn auth_policy_enforcement_role() {
        let guard = AuthGuard::<String>::bearer(vec!["token".into()])
            .with_policy(IamPolicy::RequireRole("admin".into()));
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("Bearer token".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        // Bearer-authenticated identity has no roles → policy fails
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("403")));
    }

    #[tokio::test]
    async fn auth_timing_safe_comparison() {
        // Ensure different-length tokens don't short-circuit
        let guard = AuthGuard::<String>::bearer(vec!["short".into()]);
        let mut bus = Bus::new();
        bus.insert(AuthorizationHeader("Bearer a-very-long-different-token".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    // --- ContentTypeGuard tests ---

    #[tokio::test]
    async fn content_type_json_match() {
        let guard = ContentTypeGuard::<String>::json();
        let mut bus = Bus::new();
        bus.insert(RequestContentType("application/json".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn content_type_json_with_charset() {
        let guard = ContentTypeGuard::<String>::json();
        let mut bus = Bus::new();
        bus.insert(RequestContentType("application/json; charset=utf-8".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn content_type_mismatch() {
        let guard = ContentTypeGuard::<String>::json();
        let mut bus = Bus::new();
        bus.insert(RequestContentType("text/plain".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(ref e) if e.contains("415")));
    }

    #[tokio::test]
    async fn content_type_no_header_allows() {
        let guard = ContentTypeGuard::<String>::json();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn content_type_form() {
        let guard = ContentTypeGuard::<String>::form();
        let mut bus = Bus::new();
        bus.insert(RequestContentType("application/x-www-form-urlencoded".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn content_type_accept_multiple() {
        let guard = ContentTypeGuard::<String>::accept(["application/json", "text/xml"]);
        let mut bus = Bus::new();
        bus.insert(RequestContentType("text/xml".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    // --- TimeoutGuard tests ---

    #[tokio::test]
    async fn timeout_sets_deadline() {
        let guard = TimeoutGuard::<String>::secs_30();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let deadline = bus.read::<TimeoutDeadline>().expect("deadline should be in bus");
        assert!(!deadline.is_expired());
        assert!(deadline.remaining().as_secs() >= 29);
    }

    #[tokio::test]
    async fn timeout_convenience_constructors() {
        assert_eq!(TimeoutGuard::<()>::secs_5().timeout().as_secs(), 5);
        assert_eq!(TimeoutGuard::<()>::secs_30().timeout().as_secs(), 30);
        assert_eq!(TimeoutGuard::<()>::secs_60().timeout().as_secs(), 60);
    }

    // --- IdempotencyGuard tests ---

    #[tokio::test]
    async fn idempotency_no_key_passes_through() {
        let guard = IdempotencyGuard::<String>::ttl_5min();
        let mut bus = Bus::new();
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        assert!(bus.read::<IdempotencyCachedResponse>().is_none());
    }

    #[tokio::test]
    async fn idempotency_cache_miss() {
        let guard = IdempotencyGuard::<String>::ttl_5min();
        let mut bus = Bus::new();
        bus.insert(IdempotencyKey("key-1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        assert!(bus.read::<IdempotencyCachedResponse>().is_none());
    }

    #[tokio::test]
    async fn idempotency_cache_hit() {
        let guard = IdempotencyGuard::<String>::ttl_5min();
        // Pre-populate cache
        guard.cache().insert("key-1".into(), b"cached-body".to_vec());

        let mut bus = Bus::new();
        bus.insert(IdempotencyKey("key-1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let cached = bus.read::<IdempotencyCachedResponse>().expect("cached response");
        assert_eq!(cached.body, b"cached-body");
    }

    #[tokio::test]
    async fn idempotency_cache_shared_across_clones() {
        let guard1 = IdempotencyGuard::<String>::ttl_5min();
        let guard2 = guard1.clone();
        guard1.cache().insert("shared-key".into(), b"data".to_vec());
        assert!(guard2.cache().get("shared-key").is_some());
    }

    #[tokio::test]
    async fn idempotency_expired_entry_treated_as_miss() {
        let guard = IdempotencyGuard::<String>::new(std::time::Duration::from_millis(1));
        guard.cache().insert("key-1".into(), b"old".to_vec());
        // Wait for expiry
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let mut bus = Bus::new();
        bus.insert(IdempotencyKey("key-1".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        assert!(bus.read::<IdempotencyCachedResponse>().is_none());
    }

    // --- CorsGuard additional tests ---

    #[tokio::test]
    async fn cors_guard_specific_origin_reflected() {
        let config = CorsConfig {
            allowed_origins: vec!["https://app.example.com".into()],
            ..Default::default()
        };
        let guard = CorsGuard::<String>::new(config);
        let mut bus = Bus::new();
        bus.insert(RequestOrigin("https://app.example.com".into()));
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
        let headers = bus.read::<CorsHeaders>().unwrap();
        assert_eq!(headers.access_control_allow_origin, "https://app.example.com");
    }

    #[tokio::test]
    async fn cors_guard_no_origin_passes() {
        let config = CorsConfig {
            allowed_origins: vec!["https://trusted.com".into()],
            ..Default::default()
        };
        let guard = CorsGuard::<String>::new(config);
        let mut bus = Bus::new();
        // No RequestOrigin in bus — empty origin should pass
        let result = guard.run("ok".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    // --- SecurityHeadersGuard additional tests ---

    #[tokio::test]
    async fn security_headers_custom_csp() {
        let policy = SecurityPolicy::default()
            .with_csp("default-src 'self'; script-src 'none'");
        let guard = SecurityHeadersGuard::<String>::new(policy);
        let mut bus = Bus::new();
        let _ = guard.run("ok".into(), &(), &mut bus).await;
        let headers = bus.read::<SecurityHeaders>().unwrap();
        assert_eq!(
            headers.0.content_security_policy.as_deref(),
            Some("default-src 'self'; script-src 'none'")
        );
    }

    #[tokio::test]
    async fn security_headers_default_no_csp() {
        let guard = SecurityHeadersGuard::<String>::new(SecurityPolicy::default());
        let mut bus = Bus::new();
        let _ = guard.run("ok".into(), &(), &mut bus).await;
        let headers = bus.read::<SecurityHeaders>().unwrap();
        assert!(headers.0.content_security_policy.is_none());
        assert_eq!(headers.0.referrer_policy, "strict-origin-when-cross-origin");
    }

    // --- TimeoutGuard additional test ---

    #[tokio::test]
    async fn timeout_custom_duration() {
        let guard = TimeoutGuard::<String>::new(std::time::Duration::from_millis(100));
        let mut bus = Bus::new();
        let _ = guard.run("ok".into(), &(), &mut bus).await;
        let deadline = bus.read::<TimeoutDeadline>().unwrap();
        assert!(!deadline.is_expired());
        // After sleeping past the deadline, it should be expired
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(deadline.is_expired());
    }

    // --- RateLimitGuard bucket TTL tests ---

    #[tokio::test]
    async fn rate_limit_bucket_ttl_prunes_stale_buckets() {
        // TTL of 50ms — buckets inactive for 50ms are pruned
        let guard = RateLimitGuard::<String>::new(100, 60000)
            .with_bucket_ttl(std::time::Duration::from_millis(50));

        // Create a bucket for "stale-user"
        let mut bus = Bus::new();
        bus.insert(ClientIdentity("stale-user".into()));
        let _ = guard.run("ok".into(), &(), &mut bus).await;

        // Wait for the bucket to become stale
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // Create a request from a different user — triggers lazy prune
        let mut bus2 = Bus::new();
        bus2.insert(ClientIdentity("fresh-user".into()));
        let _ = guard.run("ok".into(), &(), &mut bus2).await;

        // Now "stale-user" bucket should have been pruned.
        // Verify by exhausting "stale-user" budget — if pruned, they get fresh tokens.
        let guard2 = RateLimitGuard::<String>::new(2, 60000)
            .with_bucket_ttl(std::time::Duration::from_millis(50));

        let mut bus3 = Bus::new();
        bus3.insert(ClientIdentity("user-a".into()));
        let _ = guard2.run("1".into(), &(), &mut bus3).await;
        let _ = guard2.run("2".into(), &(), &mut bus3).await;
        // Budget exhausted
        let result = guard2.run("3".into(), &(), &mut bus3).await;
        assert!(matches!(result, Outcome::Fault(_)));

        // Wait for TTL to expire
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // Trigger prune with a different user
        let mut bus4 = Bus::new();
        bus4.insert(ClientIdentity("user-b".into()));
        let _ = guard2.run("ok".into(), &(), &mut bus4).await;

        // user-a's bucket was pruned, they get a fresh budget
        let mut bus5 = Bus::new();
        bus5.insert(ClientIdentity("user-a".into()));
        let result = guard2.run("retry".into(), &(), &mut bus5).await;
        assert!(matches!(result, Outcome::Next(_)));
    }

    #[tokio::test]
    async fn rate_limit_bucket_ttl_zero_disables_pruning() {
        // Default TTL is 0 (disabled)
        let guard = RateLimitGuard::<String>::new(2, 60000);
        assert_eq!(guard.bucket_ttl_ms(), 0);

        let mut bus = Bus::new();
        bus.insert(ClientIdentity("user".into()));
        let _ = guard.run("1".into(), &(), &mut bus).await;
        let _ = guard.run("2".into(), &(), &mut bus).await;

        // Budget exhausted — even after sleeping, no TTL prune occurs
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let result = guard.run("3".into(), &(), &mut bus).await;
        assert!(matches!(result, Outcome::Fault(_)));
    }

    #[tokio::test]
    async fn rate_limit_with_bucket_ttl_builder() {
        let guard = RateLimitGuard::<String>::new(10, 1000)
            .with_bucket_ttl(std::time::Duration::from_secs(300));
        assert_eq!(guard.bucket_ttl_ms(), 300_000);
    }

    // --- TrustedProxies tests ---

    #[test]
    fn trusted_proxies_ignores_xff_from_untrusted_direct() {
        let proxies = TrustedProxies::new(["10.0.0.1", "10.0.0.2"]);
        // Direct IP is NOT trusted — XFF should be ignored
        let result = proxies.extract("1.2.3.4, 10.0.0.1", "192.168.1.100");
        assert_eq!(result, "192.168.1.100");
    }

    #[test]
    fn trusted_proxies_extracts_rightmost_non_trusted() {
        let proxies = TrustedProxies::new(["10.0.0.1", "10.0.0.2"]);
        // Direct IP is trusted, so walk XFF right-to-left
        // XFF: "203.0.113.5, 10.0.0.2" — rightmost non-trusted is 203.0.113.5
        let result = proxies.extract("203.0.113.5, 10.0.0.2", "10.0.0.1");
        assert_eq!(result, "203.0.113.5");
    }

    #[test]
    fn trusted_proxies_multi_hop_chain() {
        let proxies = TrustedProxies::new(["10.0.0.1", "10.0.0.2", "10.0.0.3"]);
        // XFF: "real-client, 10.0.0.3, 10.0.0.2" — all hops after real-client are trusted
        let result = proxies.extract("8.8.8.8, 10.0.0.3, 10.0.0.2", "10.0.0.1");
        assert_eq!(result, "8.8.8.8");
    }

    #[test]
    fn trusted_proxies_all_xff_trusted_falls_back_to_direct() {
        let proxies = TrustedProxies::new(["10.0.0.1", "10.0.0.2"]);
        // All IPs in XFF are trusted — fall back to direct IP
        let result = proxies.extract("10.0.0.2, 10.0.0.1", "10.0.0.1");
        assert_eq!(result, "10.0.0.1");
    }

    #[test]
    fn trusted_proxies_empty_xff() {
        let proxies = TrustedProxies::new(["10.0.0.1"]);
        let result = proxies.extract("", "10.0.0.1");
        assert_eq!(result, "10.0.0.1");
    }

    #[test]
    fn trusted_proxies_is_trusted() {
        let proxies = TrustedProxies::new(["10.0.0.1", "10.0.0.2"]);
        assert!(proxies.is_trusted("10.0.0.1"));
        assert!(proxies.is_trusted("10.0.0.2"));
        assert!(!proxies.is_trusted("192.168.1.1"));
    }
}
