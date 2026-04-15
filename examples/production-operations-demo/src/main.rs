//! # Production Operations Demo
//!
//! Integrated demonstration of Ranvier's production-ready operational stack:
//!
//! | Capability          | API                              |
//! |---------------------|----------------------------------|
//! | Configuration       | `RanvierConfig::load()` / `config()` |
//! | Health checks       | `health_endpoint()` / `readiness_liveness_default()` |
//! | Request IDs         | `request_id_layer()`             |
//! | Access logging      | `AccessLogGuard` (ranvier-guard) |
//! | Prometheus metrics  | Inspector `/metrics`             |
//! | Telemetry config    | `[telemetry]` in ranvier.toml    |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p production-operations-demo
//! ```
//!
//! ## Endpoints
//!
//! | Path           | Description                       |
//! |----------------|-----------------------------------|
//! | `GET /`        | Hello response                    |
//! | `GET /orders`  | Sample order pipeline             |
//! | `GET /health`  | Health check (JSON)               |
//! | `GET /readyz`  | Readiness probe (204)             |
//! | `GET /livez`   | Liveness probe (204)              |
//! | Inspector :3001 | `/metrics` (Prometheus), `/schematic`, `/api/v1/metrics` |

use ranvier_core::config::RanvierConfig;
use ranvier_guard::prelude::*;
use ranvier_http::Ranvier;
use ranvier_inspector::Inspector;
use ranvier_macros::transition;
use ranvier_runtime::Axon;

// ── Transitions ───────────────────────────────────────────────────────────

#[transition]
async fn hello(
    _input: (),
    _res: &(),
    _bus: &mut ranvier_core::Bus,
) -> ranvier_core::Outcome<serde_json::Value, String> {
    ranvier_core::Outcome::Next(serde_json::json!({
        "service": "production-operations-demo",
        "docs": "GET /health, GET /readyz, GET /livez, Inspector :3001/metrics"
    }))
}

#[transition]
async fn order_handler(
    _input: (),
    _res: &(),
    _bus: &mut ranvier_core::Bus,
) -> ranvier_core::Outcome<serde_json::Value, String> {
    // Simulate order processing
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    ranvier_core::Outcome::Next(serde_json::json!({
        "order_id": "ORD-001",
        "status": "completed",
        "total": 99.99
    }))
}

// ── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Load configuration: ranvier.toml → profile → env vars
    let config = RanvierConfig::load()?;
    config.init_logging();

    tracing::info!(
        host = %config.server.host,
        port = %config.server.port,
        telemetry_endpoint = ?config.telemetry.otlp_endpoint,
        "configuration loaded"
    );

    // 2. Build circuits with AccessLogGuard
    let order_circuit = Axon::simple::<String>("OrderPipeline")
        .then(AccessLogGuard::new().redact_paths(vec!["/auth/login".into()]))
        .then(order_handler);

    let hello_circuit = Axon::simple::<String>("Hello")
        .then(AccessLogGuard::new())
        .then(hello);

    // 3. Clone schematic for Inspector
    let schematic = order_circuit.schematic().clone();

    // 4. Start Inspector (Prometheus /metrics + JSON /api/v1/metrics)
    let inspector = Inspector::new(schematic, config.inspector.port)
        .with_mode("dev")
        .with_auth_enforcement(false)
        .allow_unauthenticated();

    tokio::spawn(async move {
        if let Err(e) = inspector.serve().await {
            tracing::error!(error = %e, "inspector server error");
        }
    });

    // 5. Start HTTP server with full operational stack
    Ranvier::http()
        .config(&config)
        .graceful_shutdown(config.shutdown_timeout())
        .health_endpoint("/health")
        .readiness_liveness_default()
        .request_id_layer()
        .get("/", hello_circuit)
        .get("/orders", order_circuit)
        .on_start(|| {
            tracing::info!("server started — health, metrics, access logging active");
            tracing::info!("  App:       http://localhost:3000");
            tracing::info!("  Health:    http://localhost:3000/health");
            tracing::info!("  Readyz:    http://localhost:3000/readyz");
            tracing::info!("  Livez:     http://localhost:3000/livez");
            tracing::info!("  Metrics:   http://localhost:3001/metrics");
            tracing::info!("  Inspector: http://localhost:3001/quick-view");
        })
        .on_shutdown(|| {
            tracing::info!("server shut down gracefully");
        })
        .run(())
        .await
}
