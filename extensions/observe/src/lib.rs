#[cfg(feature = "otlp")]
pub mod redaction;
// Optional OTLP adapter. Provider ownership stays with the caller.
// Subscriber composition and redaction must be supplied before production use.

#[cfg(feature = "otlp")]
pub mod otlp {
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{metrics::SdkMeterProvider, trace::SdkTracerProvider, Resource};
    use std::time::Duration;

    /// Explicit transport selection; there is no fallback on invalid input.
    pub enum Protocol {
        Grpc,
        HttpProtobuf,
    }

    /// Caller-owned providers; no process-global provider is installed.
    pub struct Providers {
        /// Trace provider: redacts span/event/link attributes only; names and resource data are not filtered.
        pub traces: SdkTracerProvider,
        /// Metric provider: attribute redaction is not yet implemented; callers must supply safe labels.
        pub metrics: SdkMeterProvider,
    }

    fn validate(service: &str, endpoint: &str, timeout: Duration) -> Result<(), &'static str> {
        if service.trim().is_empty() || service.chars().any(char::is_control) || timeout.is_zero() {
            return Err("service identity and nonzero export timeout required");
        }
        if endpoint.trim() != endpoint || endpoint.chars().any(char::is_control) {
            return Err("invalid OTLP endpoint");
        }
        let url = url::Url::parse(endpoint).map_err(|_| "invalid OTLP endpoint")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("OTLP endpoint must be HTTP(S) without credentials, query or fragment");
        }
        Ok(())
    }

    impl Providers {
        pub fn build(
            service: &str,
            endpoint: &str,
            protocol: Protocol,
            redaction: crate::redaction::Mode,
            timeout: Duration,
        ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
            validate(service, endpoint, timeout)?;
            // No endpoint or credentials are included in our validation errors.
            let (traces, metrics) = match protocol {
                Protocol::Grpc => (
                    opentelemetry_otlp::SpanExporter::builder()
                        .with_tonic()
                        .with_endpoint(endpoint)
                        .with_timeout(timeout)
                        .build()?,
                    opentelemetry_otlp::MetricExporter::builder()
                        .with_tonic()
                        .with_endpoint(endpoint)
                        .with_timeout(timeout)
                        .build()?,
                ),
                Protocol::HttpProtobuf => (
                    opentelemetry_otlp::SpanExporter::builder()
                        .with_http()
                        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                        .with_endpoint(format!("{}/v1/traces", endpoint.trim_end_matches("/")))
                        .with_timeout(timeout)
                        .build()?,
                    opentelemetry_otlp::MetricExporter::builder()
                        .with_http()
                        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                        .with_endpoint(format!("{}/v1/metrics", endpoint.trim_end_matches("/")))
                        .with_timeout(timeout)
                        .build()?,
                ),
            };
            let resource = Resource::builder()
                .with_service_name(service.to_owned())
                .build();
            Ok(Self {
                traces: SdkTracerProvider::builder()
                    .with_resource(resource.clone())
                    .with_batch_exporter(crate::redaction::RedactingExporter {
                        inner: traces,
                        mode: redaction,
                    })
                    .build(),
                metrics: SdkMeterProvider::builder()
                    .with_resource(resource)
                    .with_periodic_exporter(metrics)
                    .build(),
            })
        }

        /// Attempts both providers even if the first shutdown fails.
        pub fn shutdown(&self) -> Result<(), Vec<String>> {
            let mut failures = Vec::new();
            if let Err(error) = self.traces.shutdown() {
                failures.push(error.to_string());
            }
            if let Err(error) = self.metrics.shutdown() {
                failures.push(error.to_string());
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures)
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn rejects_invalid_configuration_without_exporter_workers() {
            let timeout = Duration::from_secs(2);
            for endpoint in [
                "not a URL",
                " http://localhost:4317",
                "http://localhost:4317\n",
                "file:///tmp/export",
                "http://user:secret@localhost:4317",
                "http://localhost:4317?secret=value",
                "http://localhost:4317#secret",
            ] {
                let error = validate("fixture", endpoint, timeout).unwrap_err();
                assert!(!error.contains("secret"));
            }
            assert!(validate("   ", "http://localhost:4317", timeout).is_err());
            assert!(validate("fixture", "http://localhost:4317", Duration::ZERO).is_err());
        }
        #[test]
        fn accepts_explicit_http_and_https_destinations() {
            for endpoint in ["http://127.0.0.1:4317", "https://collector.example:443"] {
                assert!(validate("fixture", endpoint, Duration::from_secs(2)).is_ok());
            }
        }
    }
}
