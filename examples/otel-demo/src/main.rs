use async_trait::async_trait;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use ranvier_core::prelude::*;
use ranvier_observe::{init_otlp_tracing_with_protocol, init_stdout_tracing, OtlpProtocolPreset};
use ranvier_runtime::Axon;
use std::str::FromStr;
use std::time::Duration;
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

fn init_otlp_metrics(
    service_name: &str,
    endpoint: &str,
    protocol: OtlpProtocolPreset,
) -> anyhow::Result<opentelemetry_sdk::metrics::SdkMeterProvider> {
    let pipeline = opentelemetry_otlp::new_pipeline()
        .metrics(opentelemetry_sdk::runtime::Tokio)
        .with_resource(opentelemetry_sdk::Resource::new(vec![KeyValue::new(
            "service.name",
            service_name.to_string(),
        )]))
        .with_period(Duration::from_millis(300));

    let provider = match protocol {
        OtlpProtocolPreset::Grpc => pipeline
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint),
            )
            .build()?,
        OtlpProtocolPreset::HttpProtobuf => pipeline
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .http()
                    .with_endpoint(endpoint)
                    .with_protocol(Protocol::HttpBinary),
            )
            .build()?,
    };

    Ok(provider)
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

    let mut meter_provider = None;

    if let Some(endpoint) = otlp_endpoint {
        let protocol = OtlpProtocolPreset::from_str(&otlp_protocol_raw)?;
        init_otlp_tracing_with_protocol("otel-demo", &endpoint, protocol)?;
        meter_provider = Some(init_otlp_metrics("otel-demo", &endpoint, protocol)?);
        tracing::info!(
            "OTLP tracing+metrics initialized. endpoint={} protocol={}",
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
    let axon = Axon::<i32, i32, std::convert::Infallible>::new("CalculationCircuit")
        .then(AddOne)
        .then(AddOne);

    // 3. Execute
    let mut bus = Bus::new();
    let result = axon.execute(10, &(), &mut bus).await;

    let meter = opentelemetry::global::meter("otel-demo");
    let run_counter = meter
        .u64_counter("ranvier_demo_runs_total")
        .with_description("Number of otel-demo Axon execution runs")
        .init();
    let outcome_value = match &result {
        Outcome::Next(_) => "next",
        Outcome::Branch(_, _) => "branch",
        Outcome::Fault(_) => "fault",
        Outcome::Jump(_, _) => "jump",
        Outcome::Emit(_, _) => "emit",
    };
    run_counter.add(1, &[KeyValue::new("outcome", outcome_value)]);

    tracing::info!("Execution Result: {:?}", result);

    // Allow PeriodicReader to export a metrics cycle before shutdown in short-lived smoke runs.
    tokio::time::sleep(Duration::from_secs(1)).await;

    if let Some(provider) = meter_provider {
        let _ = provider.shutdown();
    }

    // Flush trace pipeline on process exit.
    opentelemetry::global::shutdown_tracer_provider();

    Ok(())
}
