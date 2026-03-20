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
//! curl -v -H "Origin: https://app.example.com" http://127.0.0.1:3456/api/hello
//!
//! # CORS rejection (untrusted origin)
//! curl -v -H "Origin: https://evil.com" http://127.0.0.1:3456/api/hello
//!
//! # OPTIONS preflight
//! curl -v -X OPTIONS -H "Origin: https://app.example.com" http://127.0.0.1:3456/api/hello
//!
//! # Security headers in response
//! curl -v http://127.0.0.1:3456/api/hello
//! ```
//!
//! ## Key Concepts
//! - `HttpIngress::guard()` auto-wires Bus injection from HTTP headers
//! - Guards execute before the circuit, rejecting with 403 on failure
//! - Response extractors apply Bus data (CORS, security headers) to responses
//! - OPTIONS preflight is auto-handled when CorsGuard is registered
//!
//! ## Prerequisites
//! - `guard-demo` — standalone Guard usage without HttpIngress

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

        Outcome::next(format!(
            "Hello from guarded API! [cors: {}, security: {}]",
            cors_origin, has_security
        ))
    }
}

// ============================================================================
// Server Setup
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt::init();

    println!("=== Guard Integration Demo ===");
    println!("Guards registered via HttpIngress::guard() — no Tower middleware needed\n");

    // Build Axon circuit
    let hello_circuit = Axon::simple::<String>("hello-api").then(HelloHandler);

    // Build guarded HTTP server
    Ranvier::http()
        .bind("127.0.0.1:3456")
        // Register guards — each auto-wires Bus injection + response extraction
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
        .guard(SecurityHeadersGuard::<()>::new(
            SecurityPolicy::new().with_csp("default-src 'self'; script-src 'self'"),
        ))
        .guard(AccessLogGuard::<()>::new())
        .on_start(|| {
            println!("Server listening on http://127.0.0.1:3456");
            println!("  GET  /api/hello  — guarded endpoint");
            println!("  Try: curl -v -H 'Origin: https://app.example.com' http://127.0.0.1:3456/api/hello");
        })
        .get("/api/hello", hello_circuit)
        .run(())
        .await
}
