//! Trace attribute policy. Resource attributes and metric labels are separate gates.
use opentelemetry::KeyValue;
use opentelemetry_sdk::{
    Resource,
    error::OTelSdkResult,
    trace::{SpanData, SpanExporter},
};
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum Mode {
    Off,
    Public,
    Strict,
}

#[derive(Debug)]
pub struct RedactingExporter<E> {
    pub(crate) inner: E,
    pub(crate) mode: Mode,
}

fn redact(attributes: &mut [KeyValue], mode: &Mode) {
    for attribute in attributes {
        let key = attribute.key.as_str().to_ascii_lowercase();
        let sensitive = key.contains("email")
            || key.contains("api_key")
            || key.contains("password")
            || key.contains("token");
        if matches!(mode, Mode::Strict) || (matches!(mode, Mode::Public) && sensitive) {
            attribute.value = "[REDACTED]".into();
        }
    }
}

impl<E: SpanExporter> SpanExporter for RedactingExporter<E> {
    async fn export(&self, mut batch: Vec<SpanData>) -> OTelSdkResult {
        for span in &mut batch {
            redact(&mut span.attributes, &self.mode);
            for event in &mut span.events.events {
                redact(&mut event.attributes, &self.mode);
            }
            for link in &mut span.links.links {
                redact(&mut link.attributes, &self.mode);
            }
        }
        self.inner.export(batch).await
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attribute_policy_replaces_sensitive_values() {
        for mode in [Mode::Public, Mode::Strict] {
            let mut attributes = vec![
                KeyValue::new("customer_email", "fixture-secret"),
                KeyValue::new("API_KEY", "fixture-secret"),
            ];
            redact(&mut attributes, &mode);
            assert!(
                attributes
                    .iter()
                    .all(|attribute| attribute.value.to_string() == "[REDACTED]")
            );
        }
    }
    #[test]
    fn off_is_explicit_and_public_retains_non_sensitive_fields() {
        let mut attributes = vec![KeyValue::new("outcome", "next")];
        redact(&mut attributes, &Mode::Public);
        assert_eq!(attributes[0].value.to_string(), "next");
        redact(&mut attributes, &Mode::Off);
        assert_eq!(attributes[0].value.to_string(), "next");
    }
    #[derive(Clone, Debug, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<SpanData>>>);
    impl SpanExporter for Capture {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.0.lock().unwrap().extend(batch);
            Ok(())
        }
    }
    #[test]
    fn sdk_export_boundary_redacts_span_event_and_link_payloads() {
        use opentelemetry::trace::{
            Span, SpanContext, SpanId, TraceFlags, TraceId, TraceState, Tracer, TracerProvider,
        };
        for mode in [Mode::Public, Mode::Strict, Mode::Off] {
            let capture = Capture::default();
            let protected = !matches!(mode, Mode::Off);
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_simple_exporter(RedactingExporter {
                    inner: capture.clone(),
                    mode,
                })
                .build();
            let tracer = provider.tracer("test");
            let mut span = tracer.start("fixture");
            span.set_attribute(KeyValue::new("customer_email", "fixture-secret"));
            span.add_event("event", vec![KeyValue::new("api_key", "fixture-secret")]);
            span.add_link(
                SpanContext::new(
                    TraceId::from(1),
                    SpanId::from(1),
                    TraceFlags::SAMPLED,
                    false,
                    TraceState::default(),
                ),
                vec![KeyValue::new("token", "fixture-secret")],
            );
            span.end();
            provider.shutdown().unwrap();
            let spans = capture.0.lock().unwrap();
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].events.events.len(), 1);
            assert_eq!(spans[0].links.links.len(), 1);
            let payload = format!("{spans:?}");
            assert_eq!(payload.contains("fixture-secret"), !protected);
            assert_eq!(payload.contains("[REDACTED]"), protected);
        }
    }
}
