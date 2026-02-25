//! # Router DSL Pack Example — Large API Demo (M151-RQ5)
//!
//! Demonstrates `RouteGroup` managing 100+ routes across multiple versioned API groups.
//!
//! **Key patterns shown:**
//! - Prefix-based grouping (`/api/v1`, `/api/v2`, `/admin`)
//! - Nested group composition (users → billing, users → preferences)
//! - Multiple HTTP methods on the same sub-path (CRUD)
//! - Route introspection via `route_descriptors()`
//!
//! Run: `cargo run -p ranvier-http --example large_api_demo`

use std::convert::Infallible;

use http::StatusCode;
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

// ── Shared result type ──────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct Ok200;

#[async_trait::async_trait]
impl Transition<(), String> for Ok200 {
    type Error = Infallible;
    type Resources = ();

    async fn run(&self, _: (), _: &(), _bus: &mut Bus) -> Outcome<String, Infallible> {
        Outcome::next("ok".to_string())
    }
}

fn handler() -> Axon<(), String, Infallible, ()> {
    Axon::new("ok").then(Ok200)
}

// ── Resource groups ─────────────────────────────────────────────────────────

/// Standard CRUD group for any resource at a given prefix/sub-path.
///
/// ```text
/// GET    {prefix}         → list
/// POST   {prefix}         → create
/// GET    {prefix}/:id     → get
/// PUT    {prefix}/:id     → update
/// DELETE {prefix}/:id     → delete
/// ```
fn crud_group(prefix: &'static str) -> RouteGroup<()> {
    RouteGroup::new(prefix)
        .get("", handler())
        .post("", handler())
        .get("/:id", handler())
        .put("/:id", handler())
        .delete("/:id", handler())
}

// ── Application Router ──────────────────────────────────────────────────────

fn build_router() -> HttpIngress<()> {
    // ── v1 API ──────────────────────────────────────
    let v1 = RouteGroup::new("/api/v1")
        // Core resources (5 routes × 4 = 20 routes)
        .group(crud_group("/users"))
        .group(crud_group("/orders"))
        .group(crud_group("/products"))
        .group(crud_group("/categories"))
        // Nested user billing sub-group (5 routes)
        .group(
            RouteGroup::new("/api/v1/users/:user_id")
                .get("/billing", handler())
                .put("/billing", handler())
                .get("/billing/invoices", handler())
                .get("/billing/invoices/:invoice_id", handler())
                .delete("/billing/invoices/:invoice_id", handler()),
        )
        // Nested user preferences sub-group (3 routes)
        .group(
            RouteGroup::new("/api/v1/users/:user_id")
                .get("/preferences", handler())
                .put("/preferences", handler())
                .delete("/preferences", handler()),
        )
        // Misc v1 endpoints (5 routes)
        .get("/search", handler())
        .get("/autocomplete", handler())
        .post("/bulk/import", handler())
        .get("/analytics/summary", handler())
        .get("/analytics/timeseries", handler());

    // ── v2 API ──────────────────────────────────────
    let v2 = RouteGroup::new("/api/v2")
        // Extended resources (5 routes × 6 = 30 routes)
        .group(crud_group("/users"))
        .group(crud_group("/orders"))
        .group(crud_group("/products"))
        .group(crud_group("/categories"))
        .group(crud_group("/reviews"))
        .group(crud_group("/coupons"))
        // Auth (4 routes)
        .post("/auth/login", handler())
        .post("/auth/logout", handler())
        .post("/auth/refresh", handler())
        .get("/auth/me", handler())
        // Webhooks (4 routes)
        .get("/webhooks", handler())
        .post("/webhooks", handler())
        .get("/webhooks/:id", handler())
        .delete("/webhooks/:id", handler());

    // ── Admin API ────────────────────────────────────
    let admin = RouteGroup::new("/admin")
        .group(crud_group("/tenants"))
        .group(crud_group("/feature-flags"))
        .get("/audit-log", handler())
        .get("/system/health", handler())
        .get("/system/metrics", handler())
        .post("/system/maintenance", handler());

    // ── Health & Misc ────────────────────────────────
    Ranvier::http::<()>()
        .route("/", handler())
        .route("/ping", handler())
        .route_group(v1)
        .route_group(v2)
        .route_group(admin)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ingress = build_router();

    // Introspect the registered route table
    let descriptors = ingress.route_descriptors();
    println!("=== Large API Demo — registered routes ({}) ===", descriptors.len());
    for desc in &descriptors {
        println!("  {:6}  {}", desc.method(), desc.path_pattern());
    }
    println!();
    println!("Routes match M151-RQ5: large-api-demo target (>= 100 routes)");
    assert!(
        descriptors.len() >= 100,
        "Expected >= 100 routes, got {}",
        descriptors.len()
    );
    println!("✓  route count OK: {}", descriptors.len());

    // Bind address check (serve not needed for the demo, just print)
    println!("\nTo start the server, call .bind(\"127.0.0.1:3000\").serve().await");
    Ok(())
}
