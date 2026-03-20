//! # Guard ↔ HttpIngress Integration
//!
//! Provides the [`GuardIntegration`] trait that bridges Guard nodes with the
//! HTTP ingress adapter, automating Bus injection from HTTP requests and
//! Bus extraction into HTTP responses.
//!
//! ## Architecture
//!
//! ```text
//! HTTP Request
//!   → BusInjectorFn (extract headers → Bus)
//!   → Guard.exec_guard (validate Bus, write Bus)
//!   → Pipeline (circuit.execute)
//!   → ResponseExtractorFn (Bus → response headers)
//!   → ResponseBodyTransformFn (Bus + body bytes → compressed body)
//! HTTP Response
//! ```
//!
//! Users register Guards via [`HttpIngress::guard()`](crate::ingress::HttpIngress::guard),
//! which auto-wires injectors, executors, and extractors.

use async_trait::async_trait;
use ranvier_core::bus::Bus;
use ranvier_core::outcome::Outcome;
use ranvier_core::transition::Transition;
use std::sync::Arc;

/// Function that extracts HTTP request data into the Bus.
pub type BusInjectorFn = Arc<dyn Fn(&http::request::Parts, &mut Bus) + Send + Sync + 'static>;

/// Function that reads Bus data and modifies response headers.
pub type ResponseExtractorFn =
    Arc<dyn Fn(&Bus, &mut http::HeaderMap) + Send + Sync + 'static>;

/// Function that transforms the response body (e.g., compression).
///
/// Takes the Bus (to read configuration like `CompressionConfig`) and the
/// original body bytes, returns the (possibly transformed) body bytes.
pub type ResponseBodyTransformFn =
    Arc<dyn Fn(&Bus, bytes::Bytes) -> bytes::Bytes + Send + Sync + 'static>;

/// A guard rejection with an HTTP status code and message.
///
/// Guards return this instead of a plain error string so the HTTP layer
/// can respond with the correct status code (e.g., 401, 403, 413, 429).
#[derive(Debug, Clone)]
pub struct GuardRejection {
    pub status: http::StatusCode,
    pub message: String,
}

impl GuardRejection {
    pub fn new(status: http::StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(http::StatusCode::FORBIDDEN, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(http::StatusCode::UNAUTHORIZED, message)
    }

    pub fn too_many_requests(message: impl Into<String>) -> Self {
        Self::new(http::StatusCode::TOO_MANY_REQUESTS, message)
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(http::StatusCode::PAYLOAD_TOO_LARGE, message)
    }
}

/// A type-erased guard executor that can run guard logic against a Bus.
///
/// Guards read from and write to the Bus. If the guard rejects the request,
/// it returns `Err(GuardRejection)` which will be converted to an HTTP error response
/// with the appropriate status code.
#[async_trait]
pub trait GuardExec: Send + Sync {
    /// Execute guard logic against the Bus.
    ///
    /// Returns `Ok(())` if the request passes, or `Err(GuardRejection)` if rejected.
    async fn exec_guard(&self, bus: &mut Bus) -> Result<(), GuardRejection>;
}

/// Wrapper that adapts a `Transition<(), ()>` Guard into a [`GuardExec`].
///
/// The `default_status` is used when the Guard's error message does not start
/// with a 3-digit HTTP status code. Guards can override by prefixing their
/// error messages with `"NNN "` (e.g., `"401 Unauthorized: missing token"`).
struct TransitionGuardExec<G> {
    guard: G,
    default_status: http::StatusCode,
}

/// Parse an optional status code prefix from a Guard error message.
///
/// If the message starts with `"NNN "` where NNN is a valid HTTP status code,
/// returns `(StatusCode, rest_of_message)`. Otherwise returns the default status.
fn parse_status_prefix(msg: &str, default: http::StatusCode) -> (http::StatusCode, String) {
    if msg.len() >= 4 && msg.as_bytes()[3] == b' ' {
        if let Ok(code) = msg[..3].parse::<u16>() {
            if let Ok(status) = http::StatusCode::from_u16(code) {
                return (status, msg[4..].to_string());
            }
        }
    }
    (default, msg.to_string())
}

#[async_trait]
impl<G> GuardExec for TransitionGuardExec<G>
where
    G: Transition<(), (), Error = String, Resources = ()> + Send + Sync + 'static,
{
    async fn exec_guard(&self, bus: &mut Bus) -> Result<(), GuardRejection> {
        match self.guard.run((), &(), bus).await {
            Outcome::Next(_) => Ok(()),
            Outcome::Fault(e) => {
                let (status, message) = parse_status_prefix(&e, self.default_status);
                Err(GuardRejection { status, message })
            }
            _ => Ok(()),
        }
    }
}

/// The complete registration bundle for a Guard integrated with HttpIngress.
pub struct RegisteredGuard {
    /// Bus injectors that extract HTTP request data for this Guard.
    pub bus_injectors: Vec<BusInjectorFn>,
    /// Optional response extractor that applies Bus data to response headers.
    pub response_extractor: Option<ResponseExtractorFn>,
    /// Optional response body transform (e.g., compression).
    pub response_body_transform: Option<ResponseBodyTransformFn>,
    /// Type-erased guard executor.
    pub exec: Arc<dyn GuardExec>,
    /// Whether this guard handles OPTIONS preflight requests.
    pub handles_preflight: bool,
    /// CORS config for preflight responses (only set by CorsGuard).
    pub preflight_config: Option<PreflightConfig>,
}

/// Configuration for automatic OPTIONS preflight responses.
#[derive(Clone)]
pub struct PreflightConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: String,
    pub allowed_headers: String,
    pub max_age: String,
    pub allow_credentials: bool,
}

/// Trait for Guards that integrate with HttpIngress.
///
/// Implementors define how HTTP request data flows into the Bus (injectors),
/// how Bus data flows into HTTP responses (extractors), and provide a
/// type-erased guard executor for the request pipeline.
///
/// # Example
///
/// ```rust,ignore
/// use ranvier_guard::CorsGuard;
///
/// Ranvier::http()
///     .guard(CorsGuard::new(CorsConfig::default()))
///     .get("/api/data", data_circuit)
///     .run(())
///     .await
/// ```
pub trait GuardIntegration: Send + Sync + 'static {
    /// Consume self and produce a complete guard registration.
    fn register(self) -> RegisteredGuard;
}

// ---------------------------------------------------------------------------
// GuardIntegration implementations for existing Guards
// ---------------------------------------------------------------------------

impl<T> GuardIntegration for ranvier_guard::CorsGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let config = self.cors_config().clone();
        let preflight = PreflightConfig {
            allowed_origins: config.allowed_origins.clone(),
            allowed_methods: config.allowed_methods.join(", "),
            allowed_headers: config.allowed_headers.join(", "),
            max_age: config.max_age_seconds.to_string(),
            allow_credentials: config.allow_credentials,
        };

        // Create a CorsGuard<()> for type-erased execution
        let exec_guard = ranvier_guard::CorsGuard::<()>::new(config);

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                if let Some(origin) = parts.headers.get("origin") {
                    if let Ok(origin_str) = origin.to_str() {
                        bus.insert(ranvier_guard::RequestOrigin(origin_str.to_string()));
                    }
                }
            })],
            response_extractor: Some(Arc::new(|bus: &Bus, headers: &mut http::HeaderMap| {
                if let Some(cors) = bus.read::<ranvier_guard::CorsHeaders>() {
                    if let Ok(v) = cors.access_control_allow_origin.parse() {
                        headers.insert("access-control-allow-origin", v);
                    }
                    if let Ok(v) = cors.access_control_allow_methods.parse() {
                        headers.insert("access-control-allow-methods", v);
                    }
                    if let Ok(v) = cors.access_control_allow_headers.parse() {
                        headers.insert("access-control-allow-headers", v);
                    }
                    if let Ok(v) = cors.access_control_max_age.parse() {
                        headers.insert("access-control-max-age", v);
                    }
                }
            })),
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::FORBIDDEN,
            }),
            handles_preflight: true,
            preflight_config: Some(preflight),
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::RateLimitGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let exec_guard = ranvier_guard::RateLimitGuard::<()>::new(
            self.max_requests(),
            self.window_ms(),
        );

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                // Use client IP or forwarded header as identity
                let identity = parts
                    .headers
                    .get("x-forwarded-for")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.split(',').next().unwrap_or("").trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                bus.insert(ranvier_guard::ClientIdentity(identity));
            })],
            response_extractor: None,
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::TOO_MANY_REQUESTS,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::SecurityHeadersGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let policy = self.policy().clone();
        let exec_guard = ranvier_guard::SecurityHeadersGuard::<()>::new(policy);

        RegisteredGuard {
            bus_injectors: vec![],
            response_extractor: Some(Arc::new(|bus: &Bus, headers: &mut http::HeaderMap| {
                if let Some(sec) = bus.read::<ranvier_guard::SecurityHeaders>() {
                    if let Ok(v) = sec.0.x_frame_options.parse() {
                        headers.insert("x-frame-options", v);
                    }
                    if let Ok(v) = sec.0.x_content_type_options.parse() {
                        headers.insert("x-content-type-options", v);
                    }
                    if let Ok(v) = sec.0.strict_transport_security.parse() {
                        headers.insert("strict-transport-security", v);
                    }
                    if let Some(ref csp) = sec.0.content_security_policy {
                        if let Ok(v) = csp.parse() {
                            headers.insert("content-security-policy", v);
                        }
                    }
                    if let Ok(v) = sec.0.x_xss_protection.parse() {
                        headers.insert("x-xss-protection", v);
                    }
                    if let Ok(v) = sec.0.referrer_policy.parse() {
                        headers.insert("referrer-policy", v);
                    }
                }
            })),
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::IpFilterGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let exec_guard = self.clone_as_unit();

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                let ip = parts
                    .headers
                    .get("x-forwarded-for")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.split(',').next().unwrap_or("").trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                bus.insert(ranvier_guard::ClientIp(ip));
            })],
            response_extractor: None,
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::FORBIDDEN,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::AccessLogGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let exec_guard = self.clone_as_unit();

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                bus.insert(ranvier_guard::AccessLogRequest {
                    method: parts.method.to_string(),
                    path: parts.uri.path().to_string(),
                });
            })],
            response_extractor: None,
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

// ---------------------------------------------------------------------------
// GuardIntegration implementations for M293 Guards
// ---------------------------------------------------------------------------

impl<T> GuardIntegration for ranvier_guard::CompressionGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let min_body_size = self.min_body_size();
        let preferred = self.preferred_encodings().to_vec();

        let mut exec_guard = ranvier_guard::CompressionGuard::<()>::new()
            .with_min_body_size(min_body_size);
        if preferred.first() == Some(&ranvier_guard::CompressionEncoding::Brotli) {
            exec_guard = exec_guard.prefer_brotli();
        }

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                if let Some(accept) = parts.headers.get("accept-encoding") {
                    if let Ok(s) = accept.to_str() {
                        bus.insert(ranvier_guard::AcceptEncoding(s.to_string()));
                    }
                }
            })],
            response_extractor: Some(Arc::new(|bus: &Bus, headers: &mut http::HeaderMap| {
                if let Some(config) = bus.read::<ranvier_guard::CompressionConfig>() {
                    if config.encoding != ranvier_guard::CompressionEncoding::Identity {
                        if let Ok(v) = config.encoding.as_str().parse() {
                            headers.insert("content-encoding", v);
                        }
                    }
                    if let Ok(v) = "accept-encoding".parse() {
                        headers.insert("vary", v);
                    }
                }
            })),
            response_body_transform: Some(Arc::new(move |bus: &Bus, body: bytes::Bytes| {
                let Some(config) = bus.read::<ranvier_guard::CompressionConfig>() else {
                    return body;
                };
                if body.len() < config.min_body_size {
                    return body;
                }
                match config.encoding {
                    ranvier_guard::CompressionEncoding::Gzip => {
                        use flate2::write::GzEncoder;
                        use flate2::Compression;
                        use std::io::Write;
                        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                        if encoder.write_all(&body).is_err() {
                            return body;
                        }
                        match encoder.finish() {
                            Ok(compressed) => bytes::Bytes::from(compressed),
                            Err(_) => body,
                        }
                    }
                    ranvier_guard::CompressionEncoding::Identity => body,
                    // Brotli/Zstd: fall through to identity (requires additional deps)
                    _ => body,
                }
            })),
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::RequestSizeLimitGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let exec_guard = ranvier_guard::RequestSizeLimitGuard::<()>::new(self.max_bytes());

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                if let Some(len) = parts.headers.get("content-length") {
                    if let Ok(s) = len.to_str() {
                        if let Ok(n) = s.parse::<u64>() {
                            bus.insert(ranvier_guard::ContentLength(n));
                        }
                    }
                }
            })],
            response_extractor: None,
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::PAYLOAD_TOO_LARGE,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::RequestIdGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let exec_guard = ranvier_guard::RequestIdGuard::<()>::new();

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                // Extract X-Request-Id from request headers if present
                if let Some(rid) = parts.headers.get("x-request-id") {
                    if let Ok(s) = rid.to_str() {
                        bus.insert(ranvier_guard::RequestId(s.to_string()));
                    }
                }
                // If no header, the Guard Transition will generate a UUID v4
            })],
            response_extractor: Some(Arc::new(|bus: &Bus, headers: &mut http::HeaderMap| {
                if let Some(rid) = bus.read::<ranvier_guard::RequestId>() {
                    if let Ok(v) = rid.0.parse() {
                        headers.insert("x-request-id", v);
                    }
                }
            })),
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::AuthGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        // Determine the header to extract based on strategy
        let header_name: &'static str = match self.strategy() {
            ranvier_guard::AuthStrategy::ApiKey { header_name, .. } => {
                // Leak a static string for the header name.
                // This is acceptable because Guards are typically created once at startup.
                Box::leak(header_name.clone().into_boxed_str())
            }
            _ => "authorization",
        };

        let exec_guard = ranvier_guard::AuthGuard::<()>::new(self.strategy().clone())
            .with_policy(self.iam_policy().clone());

        RegisteredGuard {
            bus_injectors: vec![Arc::new(move |parts: &http::request::Parts, bus: &mut Bus| {
                if let Some(value) = parts.headers.get(header_name) {
                    if let Ok(s) = value.to_str() {
                        bus.insert(ranvier_guard::AuthorizationHeader(s.to_string()));
                    }
                }
            })],
            response_extractor: Some(Arc::new(|bus: &Bus, headers: &mut http::HeaderMap| {
                // Set WWW-Authenticate on 401 responses (downstream can check IamIdentity absence)
                if bus.read::<ranvier_core::iam::IamIdentity>().is_none() {
                    if let Ok(v) = "Bearer".parse() {
                        headers.insert("www-authenticate", v);
                    }
                }
            })),
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::UNAUTHORIZED,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

// ---------------------------------------------------------------------------
// GuardIntegration implementations for M294 Guards
// ---------------------------------------------------------------------------

impl<T> GuardIntegration for ranvier_guard::ContentTypeGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let allowed_types = self.allowed_types().to_vec();
        let exec_guard = ranvier_guard::ContentTypeGuard::<()>::new(allowed_types);

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                if let Some(ct) = parts.headers.get("content-type") {
                    if let Ok(s) = ct.to_str() {
                        bus.insert(ranvier_guard::RequestContentType(s.to_string()));
                    }
                }
            })],
            response_extractor: None,
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::TimeoutGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let exec_guard = ranvier_guard::TimeoutGuard::<()>::new(self.timeout());

        RegisteredGuard {
            bus_injectors: vec![],
            response_extractor: None,
            response_body_transform: None,
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::REQUEST_TIMEOUT,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}

impl<T> GuardIntegration for ranvier_guard::IdempotencyGuard<T>
where
    T: Send + Sync + 'static,
{
    fn register(self) -> RegisteredGuard {
        let cache = self.cache().clone();
        let exec_guard = self.clone_as_unit();

        RegisteredGuard {
            bus_injectors: vec![Arc::new(|parts: &http::request::Parts, bus: &mut Bus| {
                if let Some(key) = parts.headers.get("idempotency-key") {
                    if let Ok(s) = key.to_str() {
                        bus.insert(ranvier_guard::IdempotencyKey(s.to_string()));
                    }
                }
            })],
            response_extractor: Some(Arc::new(|bus: &Bus, headers: &mut http::HeaderMap| {
                if bus.read::<ranvier_guard::IdempotencyCachedResponse>().is_some() {
                    if let Ok(v) = "true".parse() {
                        headers.insert("idempotency-replayed", v);
                    }
                }
            })),
            response_body_transform: Some(Arc::new(move |bus: &Bus, body: bytes::Bytes| {
                // Cache the response body on cache miss
                if let Some(key) = bus.read::<ranvier_guard::IdempotencyKey>() {
                    if bus.read::<ranvier_guard::IdempotencyCachedResponse>().is_none() {
                        cache.insert(key.0.clone(), body.to_vec());
                    }
                }
                body
            })),
            exec: Arc::new(TransitionGuardExec {
                guard: exec_guard,
                default_status: http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}
