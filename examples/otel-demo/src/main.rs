use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_observe::{init_otlp_tracing_with_protocol, init_stdout_tracing, OtlpProtocolPreset};
use ranvier_runtime::Axon;
use std::str::FromStr;
use tracing::instrument;

// Define a simple Transition
#[derive(Clone)]
struct AddOne;

#[async_trait]
impl Transition<i32, i32> for AddOne {
    type Error = std::convert::Infallible;
    type Resources = ();

    // Optional: Add tracing to inner logic too
    #[instrument(
        skip(self, _resources, _bus),
        fields(customer_email = "demo.user@example.com", api_key = "demo-api-key-123")
    )]
    async fn run(
        &self,
        state: i32,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<i32, Self::Error> {
        tracing::info!("Adding one to {}", state);
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        Outcome::Next(state + 1)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize tracing:
    // - default: stdout tracing
    // - optional OTLP mode when endpoint env is provided
    //
    // OTLP endpoint lookup order:
    // 1) RANVIER_OTLP_ENDPOINT
    // 2) OTEL_EXPORTER_OTLP_ENDPOINT
    //
    // OTLP protocol lookup order:
    // 1) RANVIER_OTLP_PROTOCOL
    // 2) OTEL_EXPORTER_OTLP_PROTOCOL
    // default: grpc
    let otlp_endpoint = std::env::var("RANVIER_OTLP_ENDPOINT")
        .ok()
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok());
    let otlp_protocol_raw = std::env::var("RANVIER_OTLP_PROTOCOL")
        .ok()
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").ok())
        .unwrap_or_else(|| "grpc".to_string());

    if let Some(endpoint) = otlp_endpoint {
        let protocol = OtlpProtocolPreset::from_str(&otlp_protocol_raw)?;
        init_otlp_tracing_with_protocol("otel-demo", &endpoint, protocol)?;
        tracing::info!(
            "OTLP tracing initialized. endpoint={} protocol={}",
            endpoint,
            protocol.as_str()
        );
    } else {
        init_stdout_tracing();
        tracing::info!(
            "Stdout tracing initialized (set RANVIER_OTLP_ENDPOINT and optional RANVIER_OTLP_PROTOCOL for OTLP mode)"
        );
    }

    tracing::info!("Starting Axon...");

    // 2. Define Axon
    let axon = Axon::<i32, i32, std::convert::Infallible>::start("CalculationCircuit")
        .then(AddOne)
        .then(AddOne);

    // 3. Execute
    let mut bus = Bus::new();
    let result = axon.execute(10, &(), &mut bus).await;

    tracing::info!("Execution Result: {:?}", result);

    // Flush trace pipeline on process exit.
    opentelemetry::global::shutdown_tracer_provider();

    Ok(())
}
