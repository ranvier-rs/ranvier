use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_observe::{init_otlp_tracing, init_stdout_tracing};
use ranvier_runtime::Axon;
use tracing::instrument;

// Define a simple Transition
#[derive(Clone)]
struct AddOne;

#[async_trait]
impl Transition<i32, i32> for AddOne {
    type Error = std::convert::Infallible;
    type Resources = ();

    // Optional: Add tracing to inner logic too
    #[instrument(skip(self, _resources, _bus))]
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
    let otlp_endpoint = std::env::var("RANVIER_OTLP_ENDPOINT")
        .ok()
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok());

    if let Some(endpoint) = otlp_endpoint {
        init_otlp_tracing("otel-demo", &endpoint)?;
        tracing::info!("OTLP tracing initialized. endpoint={}", endpoint);
    } else {
        init_stdout_tracing();
        tracing::info!("Stdout tracing initialized (set RANVIER_OTLP_ENDPOINT for OTLP mode)");
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
