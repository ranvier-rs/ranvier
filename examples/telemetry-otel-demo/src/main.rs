//! # Telemetry & OTLP Demo
//!
//! Demonstrates Ranvier's built-in telemetry configuration:
//!
//! 1. **RanvierConfig**: 4-layer config loading (defaults → file → profile → env)
//! 2. **TelemetryConfig**: OTLP endpoint auto-initialization
//! 3. **Structured Logging**: JSON/pretty/compact log formats
//! 4. **Config-driven HttpIngress**: `.config()` applies server settings + telemetry
//!
//! ## Prerequisites
//! - `hello-world` example
//!
//! ## Next Steps
//! - `production-operations-demo` — full production setup with metrics and access logging
//!
//! ## Running
//!
//! ```sh
//! # Default: no OTLP endpoint → telemetry is a no-op
//! cargo run -p telemetry-otel-demo
//!
//! # With OTLP endpoint (e.g., Jaeger or Grafana Tempo)
//! RANVIER_TELEMETRY__OTLP_ENDPOINT=http://localhost:4317 cargo run -p telemetry-otel-demo
//!
//! # With custom service name and sampling
//! RANVIER_TELEMETRY__SERVICE_NAME=my-api \
//! RANVIER_TELEMETRY__SAMPLE_RATIO=0.5 \
//! cargo run -p telemetry-otel-demo
//! ```

use ranvier_core::config::RanvierConfig;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;

// ── Transitions ──────────────────────────────────────────────────────────

#[transition]
async fn greet(_state: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    tracing::info!("Processing greet request");
    Outcome::Next("Hello from Ranvier with telemetry!".to_string())
}

#[transition]
async fn health_info(_state: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    let config = RanvierConfig::default();
    let info = serde_json::json!({
        "service": config.telemetry.service_name,
        "otlp_endpoint": config.telemetry.otlp_endpoint,
        "sample_ratio": config.telemetry.sample_ratio,
        "log_format": format!("{:?}", config.logging.format),
    });
    Outcome::Next(serde_json::to_string_pretty(&info).unwrap_or_default())
}

// ── Main ─────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // 1. Load configuration from 4-layer system:
    //    defaults → ranvier.toml → profile overrides → env vars
    //    Falls back to defaults if no ranvier.toml is found.
    let config = RanvierConfig::load().unwrap_or_default();

    // 2. Initialize structured logging based on config
    //    Supports: json (production), pretty (development), compact
    config.init_logging();

    // 3. Display the active telemetry configuration
    tracing::info!(
        service_name = %config.telemetry.service_name,
        otlp_endpoint = ?config.telemetry.otlp_endpoint,
        sample_ratio = config.telemetry.sample_ratio,
        otlp_protocol = ?config.telemetry.otlp_protocol,
        "Telemetry configuration loaded"
    );

    if config.telemetry.otlp_endpoint.is_some() {
        tracing::info!("OTLP exporter will be initialized by HttpIngress::config()");
    } else {
        tracing::info!("No OTLP endpoint configured — telemetry is a no-op");
    }

    // 4. Build HTTP ingress with config-driven settings
    //    .config() applies: bind address, shutdown timeout, telemetry init
    let app = Ranvier::http()
        .config(&config)
        .health_endpoint("/health")
        .readiness_liveness_default()
        .request_id_layer()
        .route("/", Axon::simple::<String>("Greet").then(greet))
        .route("/telemetry-info", Axon::simple::<String>("TelemetryInfo").then(health_info));

    tracing::info!(
        bind = %config.bind_addr(),
        "Starting server"
    );

    if let Err(e) = app.run(()).await {
        tracing::error!(error = %e, "Server error");
    }
}
