#[cfg(feature = "otlp")]
pub mod metrics;
#[cfg(feature = "otlp")]
pub mod redaction;
// Optional OTLP adapter. Provider ownership stays with the caller.
// The caller composes the layer and owns shutdown; no global installation occurs.

#[cfg(feature = "otlp")]
pub mod otlp {
    /// Attaches a caller-extracted remote parent before the tracing span starts.
    /// Rejects invalid/local identities and reports unavailable/started layers.
    /// Header parsing and trust decisions remain at the ingress adapter.
    pub fn set_remote_parent(
        span: &tracing::Span,
        parent: opentelemetry::trace::SpanContext,
    ) -> Result<(), &'static str> {
        use opentelemetry::trace::TraceContextExt;
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        if !parent.is_valid() || !parent.is_remote() {
            return Err("valid remote parent required");
        }
        span.set_parent(opentelemetry::Context::new().with_remote_span_context(parent))
            .map_err(|_| "remote parent attachment failed")
    }
    /// Captures the current tracing span and subscriber for a future.
    /// Call before spawning; every poll restores the captured context without
    /// leaving a thread-local guard alive across an await.
    pub fn with_current_context<F: std::future::Future>(
        future: F,
    ) -> impl std::future::Future<Output = F::Output> {
        use tracing::{Instrument, instrument::WithSubscriber};
        future
            .instrument(tracing::Span::current())
            .with_current_subscriber()
    }
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
        /// Trace provider: applies the configured span/event/scope/resource policy.
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
        /// Returns a layer for a caller-owned subscriber, without installing globals.
        /// Keep that subscriber alive through execution, drop its spans, then shut down.
        /// Other subscriber layers (including logging) have their own privacy policy.
        pub fn tracing_layer<S>(
            &self,
        ) -> tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>
        where
            S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
        {
            use opentelemetry::trace::TracerProvider;
            tracing_opentelemetry::layer()
                .with_tracer(self.traces.tracer("ranvier-observe"))
                .with_location(false)
                .with_threads(false)
        }

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
                    state: Default::default(),
                },
            })
        }

        /// Attempts both providers even if the first shutdown fails.
        /// This synchronous operation has no adapter-enforced overall deadline.
        /// SDK 0.32 meter shutdown ignores its timeout argument; callers must not
        /// infer worker termination or a shutdown budget from the export timeout.
        /// A blocked observable callback can outlive a Metrics failure return.
        /// Callbacks must finish cooperatively; an untrusted callback requires an
        /// externally supervised process to provide a hard termination boundary.
        pub fn shutdown(&self) -> Result<(), Vec<ShutdownSignal>> {
            shutdown_both(
                || {
                    let shutdown_ok = self.traces.shutdown().is_ok();
                    shutdown_ok && !self.trace_export_failed.load(Ordering::Acquire)
                },
                || {
                    let shutdown_ok = self.metrics.inner.shutdown().is_ok();
                    shutdown_ok && !self.metrics.state.failed()
                },
            )
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[derive(Clone, Debug, Default)]
        struct LayerCapture(Arc<std::sync::Mutex<Vec<SpanData>>>);
        impl SpanExporter for LayerCapture {
            async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
                self.0.lock().unwrap().extend(batch);
                Ok(())
            }
        }
        #[test]
        fn caller_owned_layer_preserves_parentage_and_filters_dynamic_fields() {
            use crate::redaction::{Mode, Policy, RedactingExporter};
            use tracing_subscriber::prelude::*;
            for mode in [Mode::Off, Mode::Public, Mode::Strict] {
                let capture = LayerCapture::default();
                let policy = Policy::new(mode.clone(), "private_key").unwrap();
                let providers = Providers {
                    traces: SdkTracerProvider::builder()
                        .with_simple_exporter(RedactingExporter {
                            inner: capture.clone(),
                            policy: policy.clone(),
                        })
                        .build(),
                    metrics: crate::metrics::ProtectedMeterProvider {
                        inner: SdkMeterProvider::builder().build(),
                        policy,
                        state: Default::default(),
                    },
                    trace_export_failed: Default::default(),
                };
                let subscriber = tracing_subscriber::registry().with(providers.tracing_layer());
                tracing::subscriber::with_default(subscriber, || {
                    let root = tracing::info_span!("root", customer_email = tracing::field::Empty);
                    root.record("customer_email", "layer.user@example.com");
                    let _root_guard = root.enter();
                    let child = tracing::info_span!("child", private_key = "layer-secret");
                    let _child_guard = child.enter();
                    tracing::info!(api_key = "event-secret", "safe-event");
                });
                assert_eq!(providers.shutdown(), Ok(()));
                let spans = capture.0.lock().unwrap();
                assert_eq!(spans.len(), 2);
                let child = &spans[0];
                let root = &spans[1];
                assert_eq!(child.parent_span_id, root.span_context.span_id());
                assert_eq!(child.span_context.trace_id(), root.span_context.trace_id());
                assert_eq!(child.events.len(), 1);
                let payload = format!("{spans:?}");
                for secret in ["layer.user@example.com", "layer-secret", "event-secret"] {
                    assert_eq!(payload.contains(secret), matches!(mode, Mode::Off));
                }
                assert_eq!(
                    root.name,
                    if matches!(mode, Mode::Strict) {
                        "[REDACTED]"
                    } else {
                        "root"
                    }
                );
            }
        }
        struct SuccessfulMetricExporter(Arc<std::sync::atomic::AtomicUsize>);
        #[test]
        fn tracing_layer_honors_remote_parent_sampling_and_rejects_invalid_contexts() {
            use crate::redaction::{Mode, Policy, RedactingExporter};
            use opentelemetry::trace::{
                SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
            };
            use tracing_opentelemetry::OpenTelemetrySpanExt;
            use tracing_subscriber::prelude::*;
            for ratio in [0.0, 1.0] {
                for sampled in [false, true] {
                    let capture = LayerCapture::default();
                    let policy = Policy::new(Mode::Public, "").unwrap();
                    let providers = Providers {
                        traces: SdkTracerProvider::builder()
                            .with_sampler(SampleRatio::new(ratio).unwrap().sampler())
                            .with_simple_exporter(RedactingExporter {
                                inner: capture.clone(),
                                policy: policy.clone(),
                            })
                            .build(),
                        metrics: crate::metrics::ProtectedMeterProvider {
                            inner: SdkMeterProvider::builder().build(),
                            policy,
                            state: Default::default(),
                        },
                        trace_export_failed: Default::default(),
                    };
                    let parent = SpanContext::new(
                        TraceId::from(42),
                        SpanId::from(43),
                        if sampled {
                            TraceFlags::SAMPLED
                        } else {
                            TraceFlags::default()
                        },
                        true,
                        TraceState::from_key_value([("vendor", "safe-state")]).unwrap(),
                    );
                    let subscriber = tracing_subscriber::registry().with(providers.tracing_layer());
                    tracing::subscriber::with_default(subscriber, || {
                        let ingress = tracing::info_span!("remote-ingress");
                        set_remote_parent(&ingress, parent.clone()).unwrap();
                        let context = ingress.context();
                        assert_eq!(context.span().span_context().trace_id(), parent.trace_id());
                        assert_eq!(context.span().span_context().is_sampled(), sampled);
                        assert_eq!(
                            context.span().span_context().trace_state(),
                            parent.trace_state()
                        );
                        assert!(!context.span().span_context().is_remote());
                        assert_ne!(context.span().span_context().span_id(), parent.span_id());
                        ingress.in_scope(|| {
                            let child = tracing::info_span!("remote-child");
                            assert_eq!(
                                child.context().span().span_context().trace_id(),
                                parent.trace_id()
                            );
                            assert_eq!(child.context().span().span_context().is_sampled(), sampled);
                        });
                        let conflicting = SpanContext::new(
                            TraceId::from(99),
                            SpanId::from(100),
                            TraceFlags::SAMPLED,
                            true,
                            TraceState::default(),
                        );
                        assert!(set_remote_parent(&ingress, conflicting).is_err());
                        assert_eq!(
                            ingress.context().span().span_context().trace_id(),
                            parent.trace_id()
                        );
                        assert!(set_remote_parent(&ingress, SpanContext::empty_context()).is_err());
                        let local = SpanContext::new(
                            parent.trace_id(),
                            parent.span_id(),
                            TraceFlags::SAMPLED,
                            false,
                            TraceState::default(),
                        );
                        assert!(set_remote_parent(&ingress, local).is_err());
                    });
                    assert_eq!(providers.shutdown(), Ok(()));
                    let spans = capture.0.lock().unwrap();
                    assert_eq!(spans.len(), if sampled { 2 } else { 0 });
                    if sampled {
                        let ingress = spans
                            .iter()
                            .find(|span| span.name == "remote-ingress")
                            .unwrap();
                        let child = spans
                            .iter()
                            .find(|span| span.name == "remote-child")
                            .unwrap();
                        assert_eq!(ingress.parent_span_id, parent.span_id());
                        assert_eq!(child.parent_span_id, ingress.span_context.span_id());
                    }
                    assert!(set_remote_parent(&tracing::Span::none(), parent).is_err());
                }
            }
        }
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn concurrent_spawned_tasks_keep_owned_subscribers_and_parent_contexts() {
            use crate::redaction::{Mode, Policy, RedactingExporter};
            use tracing_subscriber::prelude::*;
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let mut runs = Vec::new();
            for _ in 0..2 {
                let capture = LayerCapture::default();
                let policy = Policy::new(Mode::Public, "").unwrap();
                let providers = Providers {
                    traces: SdkTracerProvider::builder()
                        .with_simple_exporter(RedactingExporter {
                            inner: capture.clone(),
                            policy: policy.clone(),
                        })
                        .build(),
                    metrics: crate::metrics::ProtectedMeterProvider {
                        inner: SdkMeterProvider::builder().build(),
                        policy,
                        state: Default::default(),
                    },
                    trace_export_failed: Default::default(),
                };
                let dispatch = tracing::Dispatch::new(
                    tracing_subscriber::registry().with(providers.tracing_layer()),
                );
                let barrier = barrier.clone();
                let task = tracing::dispatcher::with_default(&dispatch, || {
                    let parent = tracing::info_span!("task-parent");
                    let _guard = parent.enter();
                    tokio::spawn(with_current_context(async move {
                        barrier.wait().await;
                        for _ in 0..8 {
                            tokio::task::yield_now().await;
                        }
                        let child = tracing::info_span!("task-child", api_key = "task-secret");
                        child.in_scope(|| tracing::info!("task-event"));
                    }))
                });
                // Outer caller dispatch need not remain installed on the worker.
                runs.push((providers, capture, task, dispatch));
            }
            let mut traces = Vec::new();
            for (providers, capture, task, dispatch) in runs {
                tokio::time::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .unwrap();
                let (entered, ready) = tokio::sync::oneshot::channel();
                let cancelled = tracing::dispatcher::with_default(&dispatch, || {
                    let parent = tracing::info_span!("cancelled-parent");
                    let _guard = parent.enter();
                    tokio::spawn(with_current_context(async move {
                        let _ = entered.send(());
                        std::future::pending::<()>().await;
                    }))
                });
                tokio::time::timeout(Duration::from_secs(5), ready)
                    .await
                    .unwrap()
                    .unwrap();
                cancelled.abort();
                let cancellation = tokio::time::timeout(Duration::from_secs(5), cancelled)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert!(cancellation.is_cancelled());
                drop(dispatch);
                assert_eq!(providers.shutdown(), Ok(()));
                let spans = capture.0.lock().unwrap();
                assert_eq!(spans.len(), 3);
                assert_eq!(
                    spans
                        .iter()
                        .filter(|span| span.name == "cancelled-parent")
                        .count(),
                    1
                );
                let parent = spans
                    .iter()
                    .find(|span| span.name == "task-parent")
                    .unwrap();
                let child = spans.iter().find(|span| span.name == "task-child").unwrap();
                assert_eq!(child.parent_span_id, parent.span_context.span_id());
                assert_eq!(
                    child.span_context.trace_id(),
                    parent.span_context.trace_id()
                );
                assert_eq!(child.events.len(), 1);
                assert!(!format!("{spans:?}").contains("task-secret"));
                traces.push(parent.span_context.trace_id());
            }
            assert_ne!(traces[0], traces[1]);
        }
        #[test]
        fn callback_unwind_is_classified_and_exporter_cleanup_completes() {
            use opentelemetry::metrics::MeterProvider;
            for gauge in [false, true] {
                let shutdowns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let providers = Providers {
                    traces: SdkTracerProvider::builder().build(),
                    metrics: crate::metrics::ProtectedMeterProvider {
                        inner: SdkMeterProvider::builder()
                            .with_reader(
                                opentelemetry_sdk::metrics::PeriodicReader::builder(
                                    SuccessfulMetricExporter(shutdowns.clone()),
                                )
                                .with_interval(Duration::from_secs(3600))
                                .build(),
                            )
                            .build(),
                        policy: crate::redaction::Policy::new(crate::redaction::Mode::Public, "")
                            .unwrap(),
                        state: Default::default(),
                    },
                    trace_export_failed: Arc::new(AtomicBool::new(false)),
                };
                let meter = providers.metrics.meter("panic-fixture");
                if gauge {
                    let _instrument = meter
                        .u64_observable_gauge("gauge")
                        .with_callback(|_| panic!("synthetic callback panic"))
                        .build();
                    assert_eq!(providers.shutdown(), Err(vec![ShutdownSignal::Metrics]));
                } else {
                    let _instrument = meter
                        .u64_observable_counter("counter")
                        .with_callback(|_| panic!("synthetic callback panic"))
                        .build();
                    assert_eq!(providers.shutdown(), Err(vec![ShutdownSignal::Metrics]));
                }
                assert_eq!(shutdowns.load(Ordering::Acquire), 1);
            }
        }
        impl opentelemetry_sdk::metrics::exporter::PushMetricExporter for SuccessfulMetricExporter {
            async fn export(
                &self,
                _: &opentelemetry_sdk::metrics::data::ResourceMetrics,
            ) -> OTelSdkResult {
                Ok(())
            }
            fn force_flush(&self) -> OTelSdkResult {
                Ok(())
            }
            fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
                self.0.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            fn temporality(&self) -> opentelemetry_sdk::metrics::Temporality {
                opentelemetry_sdk::metrics::Temporality::Cumulative
            }
        }
        #[test]
        fn shutdown_reports_callback_failure_even_when_sdk_exporter_succeeds() {
            use opentelemetry::metrics::MeterProvider;
            for invalid in [false, true] {
                let exporter_shutdowns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let providers = Providers {
                    traces: SdkTracerProvider::builder().build(),
                    metrics: crate::metrics::ProtectedMeterProvider {
                        inner: SdkMeterProvider::builder()
                            .with_reader(
                                opentelemetry_sdk::metrics::PeriodicReader::builder(
                                    SuccessfulMetricExporter(exporter_shutdowns.clone()),
                                )
                                .with_interval(Duration::from_secs(3600))
                                .build(),
                            )
                            .build(),
                        policy: crate::redaction::Policy::new(crate::redaction::Mode::Public, "")
                            .unwrap(),
                        state: Default::default(),
                    },
                    trace_export_failed: Arc::new(AtomicBool::new(false)),
                };
                let _counter = providers
                    .metrics
                    .meter("fixture")
                    .u64_observable_counter("counter")
                    .with_callback(move |instrument| {
                        instrument.observe(
                            if invalid { u64::MAX } else { 2 },
                            &[opentelemetry::KeyValue::new("email", "first")],
                        );
                        instrument.observe(1, &[opentelemetry::KeyValue::new("email", "second")]);
                    })
                    .build();
                assert_eq!(
                    providers.shutdown(),
                    if invalid {
                        Err(vec![ShutdownSignal::Metrics])
                    } else {
                        Ok(())
                    }
                );
                assert_eq!(exporter_shutdowns.load(Ordering::Acquire), 1);
            }
        }
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
