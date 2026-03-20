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

/// A type-erased guard executor that can run guard logic against a Bus.
///
/// Guards read from and write to the Bus. If the guard rejects the request,
/// it returns `Err(message)` which will be converted to an HTTP error response.
#[async_trait]
pub trait GuardExec: Send + Sync {
    /// Execute guard logic against the Bus.
    ///
    /// Returns `Ok(())` if the request passes, or `Err(message)` if rejected.
    async fn exec_guard(&self, bus: &mut Bus) -> Result<(), String>;
}

/// Wrapper that adapts a `Transition<(), ()>` Guard into a [`GuardExec`].
struct TransitionGuardExec<G> {
    guard: G,
}

#[async_trait]
impl<G> GuardExec for TransitionGuardExec<G>
where
    G: Transition<(), (), Error = String, Resources = ()> + Send + Sync + 'static,
{
    async fn exec_guard(&self, bus: &mut Bus) -> Result<(), String> {
        match self.guard.run((), &(), bus).await {
            Outcome::Next(_) => Ok(()),
            Outcome::Fault(e) => Err(e),
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
            exec: Arc::new(TransitionGuardExec { guard: exec_guard }),
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
            exec: Arc::new(TransitionGuardExec { guard: exec_guard }),
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
            exec: Arc::new(TransitionGuardExec { guard: exec_guard }),
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
            exec: Arc::new(TransitionGuardExec { guard: exec_guard }),
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
            exec: Arc::new(TransitionGuardExec { guard: exec_guard }),
            handles_preflight: false,
            preflight_config: None,
        }
    }
}
