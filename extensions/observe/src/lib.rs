use std::str::FromStr;

use anyhow::anyhow;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{runtime, Resource};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

/// OTLP transport preset for trace export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtlpProtocolPreset {
    Grpc,
    HttpProtobuf,
}

impl OtlpProtocolPreset {
    /// Canonical env value used in docs/examples.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grpc => "grpc",
            Self::HttpProtobuf => "http/protobuf",
        }
    }
}

impl FromStr for OtlpProtocolPreset {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "grpc" => Ok(Self::Grpc),
            "http/protobuf" | "http-protobuf" | "http_binary" | "httpbinary" | "http" => {
                Ok(Self::HttpProtobuf)
            }
            other => Err(anyhow!(
                "Unsupported OTLP protocol '{other}'. Use 'grpc' or 'http/protobuf'."
            )),
        }
    }
}

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
/// * `endpoint` - OTLP endpoint (e.g. `http://localhost:4317` for gRPC, `http://localhost:4318` for HTTP)
pub fn init_otlp_tracing(service_name: &str, endpoint: &str) -> Result<(), anyhow::Error> {
    init_otlp_tracing_with_protocol(service_name, endpoint, OtlpProtocolPreset::Grpc)
}

/// Initialize OTLP tracing with an explicit transport preset.
pub fn init_otlp_tracing_with_protocol(
    service_name: &str,
    endpoint: &str,
    protocol: OtlpProtocolPreset,
) -> Result<(), anyhow::Error> {
    let tracer = match protocol {
        OtlpProtocolPreset::Grpc => opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint),
            )
            .with_trace_config(trace_config(service_name))
            .install_batch(runtime::Tokio),
        OtlpProtocolPreset::HttpProtobuf => opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .http()
                    .with_endpoint(endpoint)
                    .with_protocol(Protocol::HttpBinary),
            )
            .with_trace_config(trace_config(service_name))
            .install_batch(runtime::Tokio),
    }?;

    // Create Layer
    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Create Filter
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ranvier_core=trace"));

    // Register Subscriber
    Registry::default()
        .with(filter)
        .with(telemetry_layer)
        // Keep stdout formatter active for local debugging and smoke-test evidence logs.
        .with(tracing_subscriber::fmt::layer())
        .init();

    Ok(())
}

fn trace_config(service_name: &str) -> opentelemetry_sdk::trace::Config {
    opentelemetry_sdk::trace::config().with_resource(Resource::new(vec![
        opentelemetry::KeyValue::new("service.name", service_name.to_string()),
    ]))
}
