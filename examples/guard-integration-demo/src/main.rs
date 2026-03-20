//! # Guard Integration Demo
//!
//! Demonstrates `HttpIngress::guard()` — the pipeline-first middleware system
//! that replaces Tower middleware with visible, traceable Guard Transition nodes.
//!
//! ## Run
//! ```bash
//! cargo run -p guard-integration-demo
//! ```
//!
//! ## Test endpoints
//! ```bash
//! # Allowed request (trusted origin)
//! curl -v -H "Origin: https://app.example.com" \
//!   -H "Authorization: Bearer demo-token" \
//!   http://127.0.0.1:3456/api/hello
//!
//! # CORS rejection (untrusted origin)
//! curl -v -H "Origin: https://evil.com" http://127.0.0.1:3456/api/hello
//!
//! # Auth rejection (missing token)
//! curl -v http://127.0.0.1:3456/api/hello
//!
//! # Compression (gzip)
//! curl -v -H "Accept-Encoding: gzip" \
//!   -H "Authorization: Bearer demo-token" \
//!   http://127.0.0.1:3456/api/hello
//!
//! # Request ID propagation
//! curl -v -H "X-Request-Id: custom-123" \
//!   -H "Authorization: Bearer demo-token" \
//!   http://127.0.0.1:3456/api/hello
//!
//! # Payload too large (will be rejected by RequestSizeLimitGuard)
//! curl -v -X POST -H "Content-Length: 99999999" \
//!   -H "Authorization: Bearer demo-token" \
//!   http://127.0.0.1:3456/api/hello
//!
//! # OPTIONS preflight
//! curl -v -X OPTIONS -H "Origin: https://app.example.com" \
//!   http://127.0.0.1:3456/api/hello
//! ```
//!
//! ## Registered Guards (9 total — Tower complete replacement)
//!
//! | Guard | Purpose | Status Code on Rejection |
//! |-------|---------|--------------------------|
//! | AccessLogGuard | Structured request logging | — (pass-through) |
//! | CorsGuard | Origin validation + CORS headers | 403 Forbidden |
//! | SecurityHeadersGuard | Standard security response headers | — (pass-through) |
//! | CompressionGuard | Accept-Encoding negotiation + gzip | — (pass-through) |
//! | RequestSizeLimitGuard | Content-Length check | 413 Payload Too Large |
//! | RequestIdGuard | X-Request-Id generation/propagation | — (pass-through) |
//! | AuthGuard | Bearer token authentication | 401 Unauthorized |
//!
//! ## Key Concepts
//! - `HttpIngress::guard()` auto-wires Bus injection from HTTP headers
//! - Guards execute before the circuit, rejecting with correct status codes
//! - Response extractors apply Bus data (CORS, security, request-id) to responses
//! - Response body transforms apply compression when negotiated
//! - OPTIONS preflight is auto-handled when CorsGuard is registered

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

// ============================================================================
// Business Logic Transition
// ============================================================================

/// Simple API handler that reads guard-injected Bus data.
#[derive(Clone)]
struct HelloHandler;

#[async_trait]
impl Transition<(), String> for HelloHandler {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // Read CORS headers injected by CorsGuard
        let cors_origin = bus
            .read::<CorsHeaders>()
            .map(|h| h.access_control_allow_origin.clone())
            .unwrap_or_else(|| "none".into());

        // Check if security headers were injected
        let has_security = bus.read::<SecurityHeaders>().is_some();

        // Read request ID from RequestIdGuard
        let request_id = bus
            .read::<RequestId>()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| "none".into());

        // Check compression config from CompressionGuard
        let compression = bus
            .read::<CompressionConfig>()
            .map(|c| c.encoding.as_str())
            .unwrap_or("none");

        // Check authenticated identity from AuthGuard
        let identity = bus
            .read::<ranvier_core::iam::IamIdentity>()
            .map(|id| id.subject.clone())
            .unwrap_or_else(|| "anonymous".into());

        Outcome::next(format!(
            "Hello from guarded API! [cors: {}, security: {}, request_id: {}, compression: {}, auth: {}]",
            cors_origin, has_security, request_id, compression, identity
        ))
    }
}

// ============================================================================
// Server Setup
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    println!("=== Guard Integration Demo (M293 — Tower Complete Replacement) ===");
    println!("9 Guards registered via HttpIngress::guard() — no Tower middleware needed\n");

    // Build Axon circuit
    let hello_circuit = Axon::simple::<String>("hello-api").then(HelloHandler);

    // Build guarded HTTP server — all 9 Guards demonstrate Tower replacement
    Ranvier::http()
        .bind("127.0.0.1:3456")
        // --- Pass-through Guards (logging, headers, negotiation) ---
        .guard(AccessLogGuard::<()>::new())
        .guard(SecurityHeadersGuard::<()>::new(
            SecurityPolicy::new().with_csp("default-src 'self'; script-src 'self'"),
        ))
        .guard(CompressionGuard::<()>::new())
        .guard(RequestIdGuard::<()>::new())
        // --- Validation Guards (reject with appropriate status codes) ---
        .guard(CorsGuard::<()>::new(CorsConfig {
            allowed_origins: vec![
                "https://app.example.com".into(),
                "https://admin.example.com".into(),
            ],
            allowed_methods: vec!["GET".into(), "POST".into(), "PUT".into(), "DELETE".into()],
            allowed_headers: vec!["Content-Type".into(), "Authorization".into()],
            max_age_seconds: 86400,
            allow_credentials: true,
        }))
        .guard(RequestSizeLimitGuard::<()>::max_2mb())
        .guard(AuthGuard::<()>::bearer(vec!["demo-token".into()]))
        .on_start(|| {
            println!("Server listening on http://127.0.0.1:3456");
            println!("  GET  /api/hello  — guarded endpoint (7 Guards active)");
            println!();
            println!("  Try: curl -v -H 'Origin: https://app.example.com' \\");
            println!("    -H 'Authorization: Bearer demo-token' \\");
            println!("    http://127.0.0.1:3456/api/hello");
        })
        .get("/api/hello", hello_circuit)
        .run(())
        .await
}
