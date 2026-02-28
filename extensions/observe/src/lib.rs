use std::borrow::Cow;
use std::collections::HashSet;
use std::pin::Pin;
use std::str::FromStr;

use anyhow::anyhow;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{global, KeyValue, Value};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::export::trace::{ExportResult, SpanData, SpanExporter};
use opentelemetry_sdk::trace::BatchSpanProcessor;
use opentelemetry_sdk::{runtime, Resource};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry};

pub mod http_metrics;
pub mod http_trace;
pub mod metrics;
pub mod business;

pub use http_metrics::{HttpMetrics, HttpMetricsLayer, HttpMetricsSnapshot, ResponseStatus};
pub use http_trace::{
    extract_trace_context, extract_trace_context_snapshot, IncomingTraceContext, TraceContextLayer,
};
pub use metrics::{Counter, Gauge, Histogram, MetricsRegistry};
pub use business::SliTracker;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TelemetryRedactionMode {
    Off,
    Public,
    Strict,
}

impl TelemetryRedactionMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "public" => Some(Self::Public),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct TelemetryRedactionPolicy {
    mode: TelemetryRedactionMode,
    sensitive_patterns: Vec<String>,
    allow_keys: HashSet<String>,
}

impl Default for TelemetryRedactionPolicy {
    fn default() -> Self {
        Self {
            mode: TelemetryRedactionMode::Off,
            sensitive_patterns: default_sensitive_patterns(),
            allow_keys: default_allow_keys(),
        }
    }
}

impl TelemetryRedactionPolicy {
    fn from_env() -> Self {
        let mut policy = Self::default();

        if let Some(raw_mode) =
            env_first(&["RANVIER_OTLP_REDACT_MODE", "RANVIER_TELEMETRY_REDACT_MODE"])
        {
            if let Some(mode) = TelemetryRedactionMode::parse(&raw_mode) {
                policy.mode = mode;
            } else {
                eprintln!(
                    "ranvier-observe: invalid redaction mode '{raw_mode}' (expected off|public|strict). Using 'off'."
                );
            }
        }

        if let Some(keys) =
            env_first(&["RANVIER_OTLP_REDACT_KEYS", "RANVIER_TELEMETRY_REDACT_KEYS"])
        {
            for pattern in parse_csv_lower(&keys) {
                if !policy.sensitive_patterns.contains(&pattern) {
                    policy.sensitive_patterns.push(pattern);
                }
            }
        }

        if let Some(keys) = env_first(&["RANVIER_OTLP_ALLOW_KEYS", "RANVIER_TELEMETRY_ALLOW_KEYS"])
        {
            policy.allow_keys.extend(parse_csv_lower(&keys));
        }

        policy
    }

    fn redact_batch(&self, batch: &mut [SpanData]) {
        if self.mode == TelemetryRedactionMode::Off {
            return;
        }

        for span in batch {
            self.redact_span(span);
        }
    }

    fn redact_span(&self, span: &mut SpanData) {
        self.redact_key_values(&mut span.attributes);

        for event in &mut span.events.events {
            self.redact_key_values(&mut event.attributes);
        }

        for link in &mut span.links.links {
            self.redact_key_values(&mut link.attributes);
        }

        // Resource is immutable; rebuild only when there are actual policy-driven mutations.
        let mut resource_kvs: Vec<KeyValue> = span
            .resource
            .iter()
            .map(|(key, value)| KeyValue {
                key: key.clone(),
                value: value.clone(),
            })
            .collect();
        let before_len = resource_kvs.len();
        self.redact_key_values(&mut resource_kvs);
        let changed = before_len != resource_kvs.len()
            || resource_kvs
                .iter()
                .zip(span.resource.iter())
                .any(|(lhs, (rhs_key, rhs_val))| {
                    lhs.key.as_str() != rhs_key.as_str() || lhs.value != *rhs_val
                });
        if changed {
            span.resource = Cow::Owned(Resource::new(resource_kvs));
        }
    }

    fn redact_key_values(&self, attributes: &mut Vec<KeyValue>) {
        if self.mode == TelemetryRedactionMode::Off {
            return;
        }

        let mut next = Vec::with_capacity(attributes.len());
        for mut kv in attributes.drain(..) {
            let lowered = kv.key.as_str().to_ascii_lowercase();

            if self.is_sensitive_key(&lowered) {
                kv.value = Value::from("[REDACTED]");
                next.push(kv);
                continue;
            }

            if self.mode == TelemetryRedactionMode::Strict && !self.is_allowed_in_strict(&lowered) {
                continue;
            }

            next.push(kv);
        }

        *attributes = next;
    }

    fn is_sensitive_key(&self, key: &str) -> bool {
        self.sensitive_patterns
            .iter()
            .any(|pattern| key.contains(pattern))
    }

    fn is_allowed_in_strict(&self, key: &str) -> bool {
        self.allow_keys.contains(key) || has_semantic_prefix(key)
    }
}

#[derive(Debug)]
struct RedactingSpanExporter<E> {
    inner: E,
    policy: TelemetryRedactionPolicy,
}

impl<E> RedactingSpanExporter<E> {
    fn new(inner: E, policy: TelemetryRedactionPolicy) -> Self {
        Self { inner, policy }
    }
}

type ExportFuture = Pin<Box<dyn std::future::Future<Output = ExportResult> + Send + 'static>>;

impl<E> SpanExporter for RedactingSpanExporter<E>
where
    E: SpanExporter + 'static,
{
    fn export(&mut self, mut batch: Vec<SpanData>) -> ExportFuture {
        self.policy.redact_batch(&mut batch);
        self.inner.export(batch)
    }

    fn shutdown(&mut self) {
        self.inner.shutdown();
    }

    fn force_flush(&mut self) -> ExportFuture {
        self.inner.force_flush()
    }
}

/// Initialize a simple stdout tracing subscriber for development
pub fn init_stdout_tracing() {
    ensure_trace_context_propagator();
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
    ensure_trace_context_propagator();

    let exporter = match protocol {
        OtlpProtocolPreset::Grpc => opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(endpoint)
            .build_span_exporter(),
        OtlpProtocolPreset::HttpProtobuf => opentelemetry_otlp::new_exporter()
            .http()
            .with_endpoint(endpoint)
            .with_protocol(Protocol::HttpBinary)
            .build_span_exporter(),
    }?;

    let redaction_policy = TelemetryRedactionPolicy::from_env();
    let tracer = build_tracer(
        service_name,
        RedactingSpanExporter::new(exporter, redaction_policy),
    );

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

fn build_tracer(
    service_name: &str,
    exporter: impl SpanExporter + 'static,
) -> opentelemetry_sdk::trace::Tracer {
    let batch_processor = BatchSpanProcessor::builder(exporter, runtime::Tokio).build();
    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_span_processor(batch_processor)
        .with_config(trace_config(service_name))
        .build();
    let tracer = provider.tracer("ranvier-observe");
    let _ = global::set_tracer_provider(provider);
    tracer
}

fn ensure_trace_context_propagator() {
    global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());
}

fn env_first(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| std::env::var(key).ok())
}

fn default_sensitive_patterns() -> Vec<String> {
    vec![
        "password".to_string(),
        "secret".to_string(),
        "token".to_string(),
        "authorization".to_string(),
        "cookie".to_string(),
        "session".to_string(),
        "api_key".to_string(),
        "credit_card".to_string(),
        "ssn".to_string(),
        "email".to_string(),
        "phone".to_string(),
    ]
}

fn default_allow_keys() -> HashSet<String> {
    HashSet::from([
        "ranvier.circuit".to_string(),
        "ranvier.node".to_string(),
        "ranvier.outcome_kind".to_string(),
        "ranvier.outcome_target".to_string(),
    ])
}

fn parse_csv_lower(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn has_semantic_prefix(key: &str) -> bool {
    const PREFIXES: [&str; 20] = [
        "service.",
        "otel.",
        "telemetry.",
        "ranvier.",
        "http.",
        "url.",
        "db.",
        "rpc.",
        "messaging.",
        "net.",
        "server.",
        "client.",
        "exception.",
        "thread.",
        "code.",
        "faas.",
        "cloud.",
        "host.",
        "os.",
        "process.",
    ];

    PREFIXES.iter().any(|prefix| key.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::{TelemetryRedactionMode, TelemetryRedactionPolicy};
    use opentelemetry::trace::{
        Event, SpanContext, SpanId, SpanKind, Status, TraceFlags, TraceId, TraceState,
    };
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::export::trace::SpanData;
    use opentelemetry_sdk::trace::{SpanEvents, SpanLinks};
    use opentelemetry_sdk::Resource;
    use std::borrow::Cow;
    use std::time::SystemTime;

    #[test]
    fn public_mode_redacts_sensitive_span_and_event_attributes() {
        let mut policy = TelemetryRedactionPolicy::default();
        policy.mode = TelemetryRedactionMode::Public;

        let mut batch = vec![sample_span_data(vec![
            KeyValue::new("customer_email", "demo.user@example.com"),
            KeyValue::new("ranvier.node", "AddOne"),
        ])];

        policy.redact_batch(&mut batch);
        let span = &batch[0];

        assert_eq!(
            attribute_value(&span.attributes, "customer_email"),
            Some("[REDACTED]".to_string())
        );
        assert_eq!(
            attribute_value(&span.attributes, "ranvier.node"),
            Some("AddOne".to_string())
        );

        let event_attributes = &span.events.events[0].attributes;
        assert_eq!(
            attribute_value(event_attributes, "api_key"),
            Some("[REDACTED]".to_string())
        );
    }

    #[test]
    fn strict_mode_drops_non_allowlisted_custom_attributes() {
        let mut policy = TelemetryRedactionPolicy::default();
        policy.mode = TelemetryRedactionMode::Strict;
        policy.allow_keys.insert("custom.keep".to_string());

        let mut batch = vec![sample_span_data(vec![
            KeyValue::new("custom.keep", "ok"),
            KeyValue::new("custom.drop", "drop-me"),
            KeyValue::new("code.namespace", "otel_demo"),
        ])];

        policy.redact_batch(&mut batch);
        let span = &batch[0];

        assert_eq!(
            attribute_value(&span.attributes, "custom.keep"),
            Some("ok".to_string())
        );
        assert_eq!(attribute_value(&span.attributes, "custom.drop"), None);
        assert_eq!(
            attribute_value(&span.attributes, "code.namespace"),
            Some("otel_demo".to_string())
        );
    }

    fn sample_span_data(attributes: Vec<KeyValue>) -> SpanData {
        let mut events = SpanEvents::default();
        events.events.push(Event::new(
            "event",
            SystemTime::now(),
            vec![KeyValue::new("api_key", "demo-api-key-123")],
            0,
        ));

        SpanData {
            span_context: SpanContext::new(
                TraceId::from_bytes([1; 16]),
                SpanId::from_bytes([2; 8]),
                TraceFlags::SAMPLED,
                false,
                TraceState::default(),
            ),
            parent_span_id: SpanId::from_bytes([0; 8]),
            span_kind: SpanKind::Internal,
            name: "test-span".into(),
            start_time: SystemTime::now(),
            end_time: SystemTime::now(),
            attributes,
            dropped_attributes_count: 0,
            events,
            links: SpanLinks::default(),
            status: Status::Unset,
            resource: Cow::Owned(Resource::new(vec![KeyValue::new(
                "service.name",
                "observe-test",
            )])),
            instrumentation_lib: opentelemetry::InstrumentationLibrary::new(
                "observe-test",
                Option::<&str>::None,
                Option::<&str>::None,
                None,
            ),
        }
    }

    fn attribute_value(attributes: &[KeyValue], key: &str) -> Option<String> {
        attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.as_str().into_owned())
    }
}
