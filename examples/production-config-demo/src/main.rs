//! # Production Config Demo
//!
//! Demonstrates Ranvier's production configuration system:
//! - `ranvier.toml` file-based configuration
//! - Environment variable overrides (`RANVIER_SERVER_PORT`, etc.)
//! - Profile system (`RANVIER_PROFILE=prod`)
//! - Structured logging (JSON/pretty/compact)
//! - Graceful shutdown with configurable timeout
//! - Health check endpoints with custom indicators
//!
//! ## Running
//!
//! ```sh
//! # Default config (reads ranvier.toml from current directory)
//! cargo run -p production-config-demo
//!
//! # Override port via environment variable
//! RANVIER_SERVER_PORT=8080 cargo run -p production-config-demo
//!
//! # Use production profile
//! RANVIER_PROFILE=prod cargo run -p production-config-demo
//! ```

use ranvier_core::config::RanvierConfig;
use ranvier_http::Ranvier;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[transition]
async fn health_status(
    _input: (),
    _res: &(),
    bus: &mut ranvier_core::Bus,
) -> ranvier_core::Outcome<serde_json::Value, String> {
    let count = bus
        .read::<Arc<AtomicU64>>()
        .map(|c| c.fetch_add(1, Ordering::Relaxed))
        .unwrap_or(0);

    ranvier_core::Outcome::Next(serde_json::json!({
        "status": "healthy",
        "version": env!("CARGO_PKG_VERSION"),
        "request_count": count
    }))
}

#[transition]
async fn hello(
    _input: (),
    _res: &(),
    _bus: &mut ranvier_core::Bus,
) -> ranvier_core::Outcome<serde_json::Value, String> {
    ranvier_core::Outcome::Next(serde_json::json!({
        "message": "Hello from Ranvier production config demo!",
        "docs": "See ranvier.toml for configuration options"
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Load configuration: ranvier.toml → profile → env vars
    let config = RanvierConfig::load()?;

    // Initialize structured logging from config
    config.init_logging();

    tracing::info!(
        host = %config.server.host,
        port = %config.server.port,
        log_format = ?config.logging.format,
        log_level = %config.logging.level,
        tls = %config.tls.enabled,
        "Configuration loaded"
    );

    let counter = Arc::new(AtomicU64::new(0));

    let hello_circuit = Axon::<(), (), String>::new("hello").then(hello);
    let status_circuit = Axon::<(), (), String>::new("status").then(health_status);

    Ranvier::http()
        .config(&config)
        .graceful_shutdown(config.shutdown_timeout())
        .health_endpoint("/health")
        .readiness_liveness_default()
        .get("/", hello_circuit)
        .get("/status", status_circuit)
        .bus_injector(move |_parts, bus| {
            bus.insert(counter.clone());
        })
        .on_start(|| {
            tracing::info!("Server started — press Ctrl+C for graceful shutdown");
        })
        .on_shutdown(|| {
            tracing::info!("Server shut down gracefully");
        })
        .run(())
        .await
}
