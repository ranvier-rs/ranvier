//! # Guard Demo
//!
//! Demonstrates the four Guard Transition nodes from `ranvier-std` that replace
//! traditional Tower/Axum middleware with visible, traceable Transition steps.
//!
//! ## Run
//! ```bash
//! cargo run -p guard-demo
//! ```
//!
//! ## Key Concepts
//! - Guard nodes are `Transition<T, T>` (pass-through on success, Fault on reject)
//! - Guards read context from the Bus (origin, IP, client identity)
//! - Guards write response headers to the Bus for the HTTP layer
//! - Chain multiple guards with `.then()` for a layered security pipeline
//!
//! ## Guards Demonstrated
//! - `CorsGuard` — validates request origin against allowed origins
//! - `RateLimitGuard` — per-client token-bucket rate limiting
//! - `SecurityHeadersGuard` — injects HSTS, CSP, X-Frame-Options into Bus
//! - `IpFilterGuard` — allow-list / deny-list IP filtering
//!
//! ## Prerequisites
//! - `hello-world` — basic Transition + Axon usage
//!
//! ## Next Steps
//! - `auth-jwt-role-demo` — JWT authentication and role-based access
//! - `multitenancy-demo` — tenant isolation patterns

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;
use ranvier_std::prelude::*;
use serde::{Deserialize, Serialize};

// ============================================================================
// Domain Types
// ============================================================================

/// Simulated HTTP request flowing through the guard pipeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HttpRequest {
    method: String,
    path: String,
    body: String,
}

/// Response after passing all guards and business logic.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

// ============================================================================
// Business Logic Transition
// ============================================================================

/// Simple handler that reads guard outputs from the Bus and returns a response.
#[derive(Clone)]
struct HelloHandler;

#[async_trait]
impl Transition<HttpRequest, HttpResponse> for HelloHandler {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        input: HttpRequest,
        _resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<HttpResponse, Self::Error> {
        // Read CORS headers injected by CorsGuard
        let cors = bus
            .read::<CorsHeaders>()
            .map(|h| format!("origin={}", h.access_control_allow_origin))
            .unwrap_or_else(|| "no-cors".into());

        // Read security headers injected by SecurityHeadersGuard
        let sec = bus
            .read::<SecurityHeaders>()
            .map(|h| format!("x-frame={}", h.0.x_frame_options))
            .unwrap_or_else(|| "no-security-headers".into());

        let body = format!(
            "Hello from {} {}! [cors: {}, security: {}]",
            input.method, input.path, cors, sec
        );

        Outcome::next(HttpResponse { status: 200, body })
    }
}

// ============================================================================
// Demo Runner
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("=== Guard Demo ===\n");

    // ── Configure Guards ─────────────────────────────────────────────────

    let cors = CorsGuard::<HttpRequest>::new(CorsConfig {
        allowed_origins: vec!["https://app.example.com".into(), "https://admin.example.com".into()],
        allowed_methods: vec!["GET".into(), "POST".into()],
        allowed_headers: vec!["Content-Type".into(), "Authorization".into()],
        max_age_seconds: 86400,
        allow_credentials: true,
    });

    let rate_limit = RateLimitGuard::<HttpRequest>::new(3, 60_000); // 3 req/min

    let security = SecurityHeadersGuard::<HttpRequest>::new(
        SecurityPolicy::new().with_csp("default-src 'self'; script-src 'self'"),
    );

    let ip_filter = IpFilterGuard::<HttpRequest>::allow_list(["127.0.0.1", "10.0.0.1", "::1"]);

    // ── Build Axon Pipeline ──────────────────────────────────────────────
    // Guards chained via .then() — each is a visible Transition node in the Schematic.

    let pipeline = Axon::<HttpRequest, HttpRequest, String>::new("Guarded API")
        .then(cors)
        .then(rate_limit)
        .then(security)
        .then(ip_filter)
        .then(HelloHandler);

    // ── Demo 1: Allowed request ──────────────────────────────────────────

    println!("--- Demo 1: Allowed request (trusted origin + allowed IP) ---");
    {
        let mut bus = Bus::new();
        bus.insert(RequestOrigin("https://app.example.com".into()));
        bus.insert(ClientIdentity("user-alice".into()));
        bus.insert(ClientIp("127.0.0.1".into()));

        let result = pipeline
            .execute(
                HttpRequest {
                    method: "GET".into(),
                    path: "/api/data".into(),
                    body: String::new(),
                },
                &(),
                &mut bus,
            )
            .await;

        match &result {
            Outcome::Next(resp) => println!("  [200] {}\n", resp.body),
            Outcome::Fault(e) => println!("  [FAULT] {}\n", e),
            other => println!("  [OTHER] {:?}\n", other),
        }
    }

    // ── Demo 2: CORS rejection ───────────────────────────────────────────

    println!("--- Demo 2: CORS rejection (untrusted origin) ---");
    {
        let mut bus = Bus::new();
        bus.insert(RequestOrigin("https://evil.com".into()));
        bus.insert(ClientIdentity("attacker".into()));
        bus.insert(ClientIp("127.0.0.1".into()));

        let result = pipeline
            .execute(
                HttpRequest {
                    method: "POST".into(),
                    path: "/api/data".into(),
                    body: "malicious".into(),
                },
                &(),
                &mut bus,
            )
            .await;

        match &result {
            Outcome::Fault(e) => println!("  [BLOCKED] {}\n", e),
            other => println!("  [UNEXPECTED] {:?}\n", other),
        }
    }

    // ── Demo 3: IP filter rejection ──────────────────────────────────────

    println!("--- Demo 3: IP filter rejection (disallowed IP) ---");
    {
        let mut bus = Bus::new();
        bus.insert(RequestOrigin("https://app.example.com".into()));
        bus.insert(ClientIdentity("remote-user".into()));
        bus.insert(ClientIp("192.168.1.100".into()));

        let result = pipeline
            .execute(
                HttpRequest {
                    method: "GET".into(),
                    path: "/api/admin".into(),
                    body: String::new(),
                },
                &(),
                &mut bus,
            )
            .await;

        match &result {
            Outcome::Fault(e) => println!("  [BLOCKED] {}\n", e),
            other => println!("  [UNEXPECTED] {:?}\n", other),
        }
    }

    // ── Demo 4: Rate limit exhaustion ────────────────────────────────────

    println!("--- Demo 4: Rate limit exhaustion (4th request exceeds 3 req/min) ---");
    {
        for i in 1..=4 {
            let mut bus = Bus::new();
            bus.insert(RequestOrigin("https://app.example.com".into()));
            bus.insert(ClientIdentity("user-bob".into()));
            bus.insert(ClientIp("127.0.0.1".into()));

            let result = pipeline
                .execute(
                    HttpRequest {
                        method: "GET".into(),
                        path: format!("/api/item/{}", i),
                        body: String::new(),
                    },
                    &(),
                    &mut bus,
                )
                .await;

            match &result {
                Outcome::Next(resp) => println!("  Request {}: [200] {}", i, resp.body),
                Outcome::Fault(e) => println!("  Request {}: [BLOCKED] {}", i, e),
                other => println!("  Request {}: [OTHER] {:?}", i, other),
            }
        }
        println!();
    }

    // ── Demo 5: Deny-list IP filter ──────────────────────────────────────

    println!("--- Demo 5: Deny-list IP filter (separate pipeline) ---");
    {
        let deny_filter = IpFilterGuard::<HttpRequest>::deny_list(["10.0.0.99", "192.168.0.1"]);
        let deny_pipeline = Axon::<HttpRequest, HttpRequest, String>::new("Deny-List API")
            .then(deny_filter)
            .then(HelloHandler);

        // Denied IP
        let mut bus = Bus::new();
        bus.insert(ClientIp("10.0.0.99".into()));
        let result = deny_pipeline
            .execute(
                HttpRequest {
                    method: "GET".into(),
                    path: "/".into(),
                    body: String::new(),
                },
                &(),
                &mut bus,
            )
            .await;
        match &result {
            Outcome::Fault(e) => println!("  Denied IP 10.0.0.99: [BLOCKED] {}", e),
            other => println!("  Denied IP 10.0.0.99: [UNEXPECTED] {:?}", other),
        }

        // Allowed IP
        let mut bus = Bus::new();
        bus.insert(ClientIp("172.16.0.5".into()));
        let result = deny_pipeline
            .execute(
                HttpRequest {
                    method: "GET".into(),
                    path: "/".into(),
                    body: String::new(),
                },
                &(),
                &mut bus,
            )
            .await;
        match &result {
            Outcome::Next(resp) => println!("  Allowed IP 172.16.0.5: [200] {}", resp.body),
            other => println!("  Allowed IP 172.16.0.5: [UNEXPECTED] {:?}", other),
        }
    }

    println!("\ndone");
    Ok(())
}
