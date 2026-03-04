//! # HTTP Observability
//!
//! Demonstrates HTTP metrics collection and distributed tracing with W3C TraceContext.
//!
//! ## Run
//! ```bash
//! cargo run -p observe-http-demo
//! ```
//!
//! ## Key Concepts
//! - HttpMetricsLayer for request metrics
//! - TraceContextLayer for W3C traceparent propagation
//! - Prometheus-compatible metrics endpoint

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_observe::{HttpMetrics, HttpMetricsLayer, TraceContextLayer, init_stdout_tracing};
use ranvier_runtime::Axon;

#[derive(Clone)]
struct AppResources {
    metrics: HttpMetrics,
}

impl ranvier_core::transition::ResourceRequirement for AppResources {}

#[derive(Clone)]
struct Ping;

#[async_trait]
impl Transition<(), String> for Ping {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        Outcome::next("pong".to_string())
    }
}

#[derive(Clone)]
struct MetricsSnapshot;

#[async_trait]
impl Transition<(), String> for MetricsSnapshot {
    type Error = String;
    type Resources = AppResources;

    async fn run(
        &self,
        _state: (),
        resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next(resources.metrics.render_prometheus())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_stdout_tracing();

    let metrics = HttpMetrics::default();
    let resources = AppResources {
        metrics: metrics.clone(),
    };

    let ping = Axon::<(), (), String, AppResources>::new("Ping").then(Ping);
    let metrics_route = Axon::<(), (), String, AppResources>::new("Metrics").then(MetricsSnapshot);

    println!("observe-http-demo listening on http://127.0.0.1:3140");
    println!("try: curl http://127.0.0.1:3140/ping");
    println!("try: curl http://127.0.0.1:3140/metrics");
    println!(
        "try: curl -H \"traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01\" http://127.0.0.1:3140/ping"
    );

    Ranvier::http::<AppResources>()
        .bind("127.0.0.1:3140")
        .layer(TraceContextLayer::new())
        .layer(HttpMetricsLayer::new(metrics))
        .get("/ping", ping)
        .get("/metrics", metrics_route)
        .run(resources)
        .await
}
