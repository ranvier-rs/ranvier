use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::Tracer;
use opentelemetry_sdk::{runtime, Resource};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

/// Initialize a simple stdout tracing subscriber for development
pub fn init_stdout_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ranvier_core=debug"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Initialize OTLP tracing (connects to Jaeger/Collector)
///
/// # Arguments
/// * `service_name` - The name of the service (e.g. "order-service")
/// * `endpoint` - OTLP gRPC endpoint (e.g. "http://localhost:4317")
pub fn init_otlp_tracing(service_name: &str, endpoint: &str) -> Result<(), anyhow::Error> {
    // 1. Create OTLP exporter
    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint);

    // 2. Create Trace Config with Resource
    let trace_config = opentelemetry_sdk::trace::config().with_resource(Resource::new(vec![
        opentelemetry::KeyValue::new("service.name", service_name.to_string()),
    ]));

    // 3. Create Tracer
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .with_trace_config(trace_config)
        .install_batch(runtime::Tokio)?;

    // 4. Create Layer
    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // 5. Create Filter
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ranvier_core=trace"));

    // 6. Register Subscriber
    Registry::default()
        .with(filter)
        .with(telemetry_layer)
        .with(tracing_subscriber::fmt::layer()) // Start with stdout too for debug
        .init();

    Ok(())
}
