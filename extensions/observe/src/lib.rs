#[cfg(feature = "otlp")]
pub mod metrics;
#[cfg(feature = "otlp")]
pub mod redaction;
// Optional OTLP adapter. Provider ownership stays with the caller.
// Subscriber composition and redaction must be supplied before production use.

#[cfg(feature = "otlp")]
pub mod otlp {
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider, trace::SdkTracerProvider};
    use opentelemetry_sdk::{
        error::OTelSdkResult,
        trace::{SpanData, SpanExporter},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    #[derive(Debug)]
    struct ObservedExporter<E> {
        inner: E,
        failed: Arc<AtomicBool>,
    }
    impl<E: SpanExporter> SpanExporter for ObservedExporter<E> {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            let result = self.inner.export(batch).await;
            if result.is_err() {
                self.failed.store(true, Ordering::Release);
            }
            result
        }
        fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
            self.inner.shutdown_with_timeout(timeout)
        }
        fn force_flush(&self) -> OTelSdkResult {
            self.inner.force_flush()
        }
        fn set_resource(&mut self, resource: &Resource) {
            self.inner.set_resource(resource);
        }
    }

    /// Explicit transport selection; there is no fallback on invalid input.
    pub enum Protocol {
        Grpc,
        HttpProtobuf,
    }

    /// Explicit root-span ratio; incoming parent sampling decisions are preserved.
    #[derive(Clone, Copy, Debug)]
    pub struct SampleRatio(f64);
    impl SampleRatio {
        pub fn new(value: f64) -> Result<Self, &'static str> {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err("sample ratio must be finite and within 0..=1");
            }
            Ok(Self(value))
        }
        fn sampler(self) -> opentelemetry_sdk::trace::Sampler {
            opentelemetry_sdk::trace::Sampler::ParentBased(Box::new(
                opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(self.0),
            ))
        }
    }

    /// Signal whose provider shutdown failed, or whose trace export failed earlier.
    /// Backend error bodies are deliberately excluded from this public result.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum ShutdownSignal {
        Traces,
        Metrics,
    }

    fn shutdown_both(
        mut traces: impl FnMut() -> bool,
        mut metrics: impl FnMut() -> bool,
    ) -> Result<(), Vec<ShutdownSignal>> {
        let mut failures = Vec::new();
        if !traces() {
            failures.push(ShutdownSignal::Traces);
        }
        if !metrics() {
            failures.push(ShutdownSignal::Metrics);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }
    /// Caller-owned providers; no process-global provider is installed.
    pub struct Providers {
        /// Trace provider: redacts span/event/link/resource attributes; names are not filtered.
        pub traces: SdkTracerProvider,
        /// Metric provider: filters resource and record/observe labels; metadata is a separate policy gate.
        pub metrics: crate::metrics::ProtectedMeterProvider,
        trace_export_failed: Arc<AtomicBool>,
    }

    /// Maximum request budget for this initial adapter; longer requests need a reviewed lifecycle policy.
    pub const MAX_EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

    fn validate(service: &str, endpoint: &str, timeout: Duration) -> Result<(), &'static str> {
        if service.trim().is_empty() || service.chars().any(char::is_control) || timeout.is_zero() {
            return Err("service identity and nonzero export timeout required");
        }
        if endpoint.trim() != endpoint || endpoint.chars().any(char::is_control) {
            return Err("invalid OTLP endpoint");
        }
        if timeout > MAX_EXPORT_TIMEOUT {
            return Err("export timeout exceeds adapter maximum");
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
            redaction: crate::redaction::Policy,
            sample_ratio: SampleRatio,
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
            let resource = redaction.resource(
                &Resource::builder()
                    .with_service_name(service.to_owned())
                    .build(),
            );
            let trace_export_failed = Arc::new(AtomicBool::new(false));
            Ok(Self {
                traces: SdkTracerProvider::builder()
                    .with_sampler(sample_ratio.sampler())
                    .with_resource(resource.clone())
                    .with_batch_exporter(crate::redaction::RedactingExporter {
                        inner: ObservedExporter {
                            inner: traces,
                            failed: trace_export_failed.clone(),
                        },
                        policy: redaction.clone(),
                    })
                    .build(),
                trace_export_failed,
                metrics: crate::metrics::ProtectedMeterProvider {
                    inner: SdkMeterProvider::builder()
                        .with_resource(resource)
                        .with_periodic_exporter(metrics)
                        .build(),
                    policy: redaction,
                },
            })
        }

        /// Attempts both providers even if the first shutdown fails.
        /// This synchronous operation has no adapter-enforced overall deadline.
        /// SDK 0.32 meter shutdown ignores its timeout argument; callers must not
        /// infer worker termination or a shutdown budget from the export timeout.
        pub fn shutdown(&self) -> Result<(), Vec<ShutdownSignal>> {
            shutdown_both(
                || {
                    let shutdown_ok = self.traces.shutdown().is_ok();
                    shutdown_ok && !self.trace_export_failed.load(Ordering::Acquire)
                },
                || {
                    let shutdown_ok = self.metrics.inner.shutdown().is_ok();
                    shutdown_ok && !self.metrics.policy.metric_failed()
                },
            )
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn ratio_rejects_nonfinite_and_out_of_range_values() {
            for ratio in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01] {
                assert!(SampleRatio::new(ratio).is_err());
            }
            for ratio in [0.0, 0.25, 1.0] {
                assert!(SampleRatio::new(ratio).is_ok());
            }
        }
        #[test]
        fn root_ratio_boundaries_and_remote_parent_decisions_are_explicit() {
            use opentelemetry::{
                Context,
                trace::{
                    Span, SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
                    Tracer, TracerProvider,
                },
            };
            for ratio in [0.0, 1.0] {
                let provider = SdkTracerProvider::builder()
                    .with_sampler(SampleRatio::new(ratio).unwrap().sampler())
                    .with_simple_exporter(AlternatingExporter(AtomicBool::new(false)))
                    .build();
                let tracer = provider.tracer("ratio-fixture");
                let mut root = tracer.start_with_context("root", &Context::new());
                assert_eq!(root.is_recording(), ratio == 1.0);
                root.end();
                for sampled in [false, true] {
                    let parent = Context::new().with_remote_span_context(SpanContext::new(
                        TraceId::from(1),
                        SpanId::from(1),
                        if sampled {
                            TraceFlags::SAMPLED
                        } else {
                            TraceFlags::default()
                        },
                        true,
                        TraceState::default(),
                    ));
                    let mut child = tracer.start_with_context("child", &parent);
                    assert_eq!(child.is_recording(), sampled);
                    assert_eq!(child.span_context().is_sampled(), sampled);
                    child.end();
                }
                provider.shutdown().unwrap();
            }
        }
        #[derive(Debug)]
        struct AlternatingExporter(AtomicBool);
        impl SpanExporter for AlternatingExporter {
            async fn export(&self, _: Vec<SpanData>) -> OTelSdkResult {
                if self.0.swap(false, Ordering::AcqRel) {
                    Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                        "synthetic secret".into(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
        #[tokio::test]
        async fn export_failure_remains_observable_after_later_success() {
            let failed = Arc::new(AtomicBool::new(false));
            let exporter = ObservedExporter {
                inner: AlternatingExporter(AtomicBool::new(true)),
                failed: failed.clone(),
            };
            assert!(exporter.export(Vec::new()).await.is_err());
            assert!(failed.load(Ordering::Acquire));
            assert!(exporter.export(Vec::new()).await.is_ok());
            assert!(failed.load(Ordering::Acquire));
            assert!(
                exporter
                    .shutdown_with_timeout(Duration::from_secs(1))
                    .is_ok()
            );
            assert!(failed.load(Ordering::Acquire));
        }
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
        #[test]
        fn export_timeout_has_an_explicit_upper_bound() {
            assert!(validate("fixture", "http://localhost:4317", MAX_EXPORT_TIMEOUT).is_ok());
            assert!(
                validate(
                    "fixture",
                    "http://localhost:4317",
                    MAX_EXPORT_TIMEOUT + Duration::from_nanos(1)
                )
                .is_err()
            );
        }
        #[test]
        fn shutdown_attempts_both_signals_and_reports_only_classification() {
            let metrics_called = std::cell::Cell::new(false);
            let result = shutdown_both(
                || false,
                || {
                    metrics_called.set(true);
                    false
                },
            );
            assert!(metrics_called.get());
            assert_eq!(
                result,
                Err(vec![ShutdownSignal::Traces, ShutdownSignal::Metrics])
            );
            assert_eq!(shutdown_both(|| true, || true), Ok(()));
            assert_eq!(
                shutdown_both(|| false, || true),
                Err(vec![ShutdownSignal::Traces])
            );
            assert_eq!(
                shutdown_both(|| true, || false),
                Err(vec![ShutdownSignal::Metrics])
            );
        }
    }
}
