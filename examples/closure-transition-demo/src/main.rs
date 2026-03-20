//! # Closure Transition Demo
//!
//! Demonstrates Ranvier's closure-based inline transitions (`then_fn()`) mixed
//! with traditional `#[transition]` macro steps in the same pipeline.
//!
//! ## Key Features
//! - `Axon::then_fn()` for lightweight data transformations
//! - `Axon::typed::<T, E>()` for typed-input pipelines
//! - `#[transition]` macro for complex business logic
//! - `HttpIngress::post_typed()` for type-safe body injection
//! - Mixed closure + macro transitions in a single pipeline chain
//!
//! ## Run
//! ```bash
//! cargo run -p closure-transition-demo
//! ```
//!
//! ## Endpoints
//! - POST /greet       — greeting pipeline (closure-only)
//! - POST /transform   — data transform pipeline (mixed: closure + macro)
//! - GET  /health      — health check (closure-only)
//!
//! ## Test
//! ```bash
//! curl -X POST http://localhost:3000/greet \
//!   -H "Content-Type: application/json" \
//!   -d '{"name":"World"}'
//!
//! curl -X POST http://localhost:3000/transform \
//!   -H "Content-Type: application/json" \
//!   -d '{"text":"hello ranvier","multiplier":3}'
//! ```

use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_http::Ranvier;
use ranvier_macros::transition;
use ranvier_runtime::Axon;

// ── Typed request models ────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct GreetRequest {
    name: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct TransformRequest {
    text: String,
    multiplier: usize,
}

// ── Traditional macro transition (complex step) ─────────────────────

#[transition]
async fn repeat_text(
    input: String,
    _res: &(),
    bus: &mut Bus,
) -> Outcome<serde_json::Value, String> {
    let multiplier = bus
        .read::<usize>()
        .copied()
        .unwrap_or(1);

    let repeated = input.repeat(multiplier);
    let char_count = repeated.len();

    Outcome::Next(serde_json::json!({
        "original": input,
        "repeated": repeated,
        "char_count": char_count,
        "multiplier": multiplier,
    }))
}

// ── Pipelines ───────────────────────────────────────────────────────

/// All-closure pipeline: greeting with formatting.
///
/// Uses `Axon::typed::<In, E>()` to create a pipeline that accepts
/// `GreetRequest` directly from `post_typed()`.
fn greet_pipeline() -> Axon<GreetRequest, serde_json::Value, String> {
    Axon::typed::<GreetRequest, String>("greet")
        .then_fn("validate", |req: GreetRequest, _bus: &mut Bus| {
            if req.name.is_empty() {
                Outcome::Fault("Name cannot be empty".to_string())
            } else {
                Outcome::Next(req.name)
            }
        })
        .then_fn("format-greeting", |name: String, _bus: &mut Bus| {
            Outcome::Next(serde_json::json!({
                "message": format!("Hello, {}!", name),
                "length": name.len(),
            }))
        })
}

/// Mixed pipeline: closure steps + macro transition in one chain.
///
/// - Step 1 (closure): extract text and store multiplier in Bus
/// - Step 2 (macro):   repeat text using Bus-stored multiplier
fn transform_pipeline() -> Axon<TransformRequest, serde_json::Value, String> {
    Axon::typed::<TransformRequest, String>("transform")
        .then_fn("extract-and-prepare", |req: TransformRequest, bus: &mut Bus| {
            bus.insert(req.multiplier);
            Outcome::Next(req.text.to_uppercase())
        })
        .then(repeat_text)
}

/// Single-step closure pipeline for health checks.
fn health_pipeline() -> Axon<(), serde_json::Value, String> {
    Axon::simple::<String>("health")
        .then_fn("health-check", |_input: (), _bus: &mut Bus| {
            Outcome::Next(serde_json::json!({ "status": "ok" }))
        })
}

// ── Main ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    println!("Closure Transition Demo starting on {addr}");
    println!("  POST /greet       — greeting pipeline (closure-only)");
    println!("  POST /transform   — mixed closure + macro pipeline");
    println!("  GET  /health      — health check (closure)");

    Ranvier::http()
        .bind(&addr)
        .post_typed("/greet", greet_pipeline())
        .post_typed("/transform", transform_pipeline())
        .get("/health", health_pipeline())
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
