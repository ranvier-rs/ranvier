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
//!
//! # POST with wrong Content-Type (per-route ContentTypeGuard)
//! curl -v -X POST -H "Content-Type: text/plain" \
//!   -H "Authorization: Bearer demo-token" \
//!   -d '{"name":"test"}' \
//!   http://127.0.0.1:3456/api/orders
//!
//! # POST with Idempotency-Key (per-route IdempotencyGuard)
//! curl -v -X POST -H "Content-Type: application/json" \
//!   -H "Authorization: Bearer demo-token" \
//!   -H "Idempotency-Key: order-abc-123" \
//!   -d '{"name":"test"}' \
//!   http://127.0.0.1:3456/api/orders
//! ```
//!
//! ## Registered Guards
//!
//! ### Global Guards (7)
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
//! ### Per-Route Guards (POST /api/orders only)
//!
//! | Guard | Purpose | Status Code on Rejection |
//! |-------|---------|--------------------------|
//! | TimeoutGuard | 30-second pipeline deadline | 408 Request Timeout |
//! | ContentTypeGuard | Require application/json | 415 Unsupported Media Type |
//! | IdempotencyGuard | Duplicate request prevention | — (cache replay) |
//!
//! ## Key Concepts
//! - `HttpIngress::guard()` auto-wires Bus injection from HTTP headers
//! - Guards execute before the circuit, rejecting with correct status codes
//! - Response extractors apply Bus data (CORS, security, request-id) to responses
//! - Response body transforms apply compression when negotiated
//! - OPTIONS preflight is auto-handled when CorsGuard is registered
//! - `post_with_guards()` + `guards![]` macro for per-route Guard composition
//! - TimeoutGuard → ingress enforces via `tokio::time::timeout()`
//! - IdempotencyGuard → cache hit skips circuit, cache miss caches response

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_guard::prelude::*;
use ranvier_http::guards;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use std::time::Duration;

// ============================================================================
// Business Logic Transitions
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
            .get_cloned::<CorsHeaders>()
            .map(|h| h.access_control_allow_origin)
            .unwrap_or_else(|_| "none".into());

        // Check if security headers were injected
        let has_security = bus.get_cloned::<SecurityHeaders>().is_ok();

        // Read request ID from RequestIdGuard
        let request_id = bus
            .get_cloned::<RequestId>()
            .map(|r| r.0)
            .unwrap_or_else(|_| "none".into());

        // Check compression config from CompressionGuard
        let compression = bus
            .get_cloned::<CompressionConfig>()
            .map(|c| c.encoding.as_str().to_string())
            .unwrap_or_else(|_| "none".into());

        // Check authenticated identity from AuthGuard
        let identity = bus
            .get_cloned::<ranvier_core::iam::IamIdentity>()
            .map(|id| id.subject)
            .unwrap_or_else(|_| "anonymous".into());

        Outcome::next(format!(
            "Hello from guarded API! [cors: {}, security: {}, request_id: {}, compression: {}, auth: {}]",
            cors_origin, has_security, request_id, compression, identity
        ))
    }
}

/// Order creation handler — used for per-route Guard demo.
#[derive(Clone)]
struct CreateOrderHandler;

#[async_trait]
impl Transition<(), String> for CreateOrderHandler {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        // Check if we have a timeout deadline
        let timeout_remaining = bus
            .get_cloned::<TimeoutDeadline>()
            .map(|td| format!("{}s", td.remaining().as_secs()))
            .unwrap_or_else(|_| "none".into());

        // Check idempotency key
        let idem_key = bus
            .get_cloned::<IdempotencyKey>()
            .map(|k| k.0)
            .unwrap_or_else(|_| "none".into());

        Outcome::next(format!(
            "{{\"order_id\": \"ord-001\", \"status\": \"created\", \"timeout_remaining\": \"{}\", \"idempotency_key\": \"{}\"}}",
            timeout_remaining, idem_key
        ))
    }
}

// ============================================================================
// Server Setup
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    println!("=== Guard Integration Demo (M294 — per-route Guards + Timeout + Idempotency) ===");
    println!("12 Guards total: 7 global + 3 per-route on POST /api/orders\n");

    // Build Axon circuits
    let hello_circuit = Axon::simple::<String>("hello-api").then(HelloHandler);
    let order_circuit = Axon::simple::<String>("create-order").then(CreateOrderHandler);

    // Build guarded HTTP server — all Guards demonstrate Tower replacement
    Ranvier::http()
        .bind("127.0.0.1:3456")
        // --- Global Guards (7) ---
        .guard(AccessLogGuard::<()>::new())
        .guard(SecurityHeadersGuard::<()>::new(
            SecurityPolicy::new().with_csp("default-src 'self'; script-src 'self'"),
        ))
        .guard(CompressionGuard::<()>::new())
        .guard(RequestIdGuard::<()>::new())
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
            println!("  GET  /api/hello   — 7 global Guards");
            println!("  POST /api/orders  — 7 global + 3 per-route Guards (Timeout + ContentType + Idempotency)");
            println!();
            println!("  Try: curl -v -H 'Origin: https://app.example.com' \\");
            println!("    -H 'Authorization: Bearer demo-token' \\");
            println!("    http://127.0.0.1:3456/api/hello");
            println!();
            println!("  Try: curl -v -X POST -H 'Content-Type: application/json' \\");
            println!("    -H 'Authorization: Bearer demo-token' \\");
            println!("    -H 'Idempotency-Key: test-123' \\");
            println!("    -d '{{}}' http://127.0.0.1:3456/api/orders");
        })
        .get("/api/hello", hello_circuit)
        // --- Per-Route Guards: POST /api/orders ---
        // TimeoutGuard (30s), ContentTypeGuard (json), IdempotencyGuard (5min)
        .post_with_guards("/api/orders", order_circuit, guards![
            TimeoutGuard::<()>::new(Duration::from_secs(30)),
            ContentTypeGuard::<()>::json(),
            IdempotencyGuard::<()>::ttl_5min(),
        ])
        .run(())
        .await
}
