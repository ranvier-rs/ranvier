//! Optional OTLP adapter. Provider ownership stays with the caller.
//! Subscriber composition and redaction must be supplied before production use.

#[cfg(feature = "otlp")]
pub mod otlp {
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{metrics::SdkMeterProvider, trace::SdkTracerProvider, Resource};
    use std::time::Duration;

    /// Explicit transport selection; there is no fallback on invalid input.
    pub enum Protocol { Grpc, HttpProtobuf }

    /// Caller-owned providers; no process-global provider is installed.
    pub struct Providers {
        pub traces: SdkTracerProvider,
        pub metrics: SdkMeterProvider,
    }

    impl Providers {
        pub fn build(service: &str, endpoint: &str, protocol: Protocol, timeout: Duration)
            -> Result<Self, Box<dyn std::error::Error + Send + Sync>>
        {
            if service.trim().is_empty() || timeout.is_zero() {
                return Err("service identity and nonzero export timeout required".into());
            }
            // SDK builders validate the endpoint. Errors propagate without logging it.
            let (traces, metrics) = match protocol {
                Protocol::Grpc => (
                    opentelemetry_otlp::SpanExporter::builder().with_tonic().with_endpoint(endpoint).with_timeout(timeout).build()?,
                    opentelemetry_otlp::MetricExporter::builder().with_tonic().with_endpoint(endpoint).with_timeout(timeout).build()?,
                ),
                Protocol::HttpProtobuf => (
                    opentelemetry_otlp::SpanExporter::builder().with_http().with_protocol(opentelemetry_otlp::Protocol::HttpBinary).with_endpoint(endpoint).with_timeout(timeout).build()?,
                    opentelemetry_otlp::MetricExporter::builder().with_http().with_protocol(opentelemetry_otlp::Protocol::HttpBinary).with_endpoint(endpoint).with_timeout(timeout).build()?,
                ),
            };
            let resource = Resource::builder().with_service_name(service.to_owned()).build();
            Ok(Self {
                traces: SdkTracerProvider::builder().with_resource(resource.clone()).with_batch_exporter(traces).build(),
                metrics: SdkMeterProvider::builder().with_resource(resource).with_periodic_exporter(metrics).build(),
            })
        }

        /// Attempts both providers even if the first shutdown fails.
        pub fn shutdown(&self) -> Result<(), Vec<String>> {
            let mut failures = Vec::new();
            if let Err(error) = self.traces.shutdown() { failures.push(error.to_string()); }
            if let Err(error) = self.metrics.shutdown() { failures.push(error.to_string()); }
            if failures.is_empty() { Ok(()) } else { Err(failures) }
        }
    }
}