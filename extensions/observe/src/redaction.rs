//! Trace and resource attribute policy. Metric data labels remain a separate gate.
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

/// Validated additive, case-insensitive substring keys. Built-in keys always remain active.
#[derive(Clone, Debug)]
pub struct Policy {
    mode: Mode,
    keys: Vec<String>,
}
impl Policy {
    /// Empty input adds no keys. Nonempty lists require 1..64 ASCII key fragments,
    /// each at most 128 bytes, using letters, digits, dot, underscore or hyphen.
    pub fn new(mode: Mode, additional_keys: &str) -> Result<Self, &'static str> {
        if additional_keys.len() > 8255 || additional_keys.chars().any(char::is_control) {
            return Err("invalid additional redaction keys");
        }
        let mut keys = Vec::new();
        if !additional_keys.trim().is_empty() {
            for value in additional_keys.split(',') {
                let value = value.trim();
                if keys.len() >= 64
                    || value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                {
                    return Err("invalid additional redaction keys");
                }
                keys.push(value.to_ascii_lowercase());
            }
        }
        Ok(Self { mode, keys })
    }

    pub(crate) fn resource(&self, resource: &Resource) -> Resource {
        if matches!(self.mode, Mode::Off) {
            return resource.clone();
        }
        let mut attributes: Vec<KeyValue> = resource
            .iter()
            .map(|(key, value)| KeyValue::new(key.clone(), value.clone()))
            .collect();
        redact(&mut attributes, self);
        // Do not rerun environment/default detectors or forward an arbitrary schema URL.
        Resource::builder_empty()
            .with_attributes(attributes)
            .build()
    }
}

#[derive(Debug)]
pub struct RedactingExporter<E> {
    pub(crate) inner: E,
    pub(crate) policy: Policy,
}

fn redact(attributes: &mut [KeyValue], policy: &Policy) {
    for attribute in attributes {
        let key = attribute.key.as_str().to_ascii_lowercase();
        let sensitive = key.contains("email")
            || key.contains("api_key")
            || key.contains("password")
            || key.contains("token")
            || policy.keys.iter().any(|fragment| key.contains(fragment));
        if matches!(policy.mode, Mode::Strict) || (matches!(policy.mode, Mode::Public) && sensitive)
        {
            attribute.value = "[REDACTED]".into();
        }
    }
}

impl<E: SpanExporter> SpanExporter for RedactingExporter<E> {
    async fn export(&self, mut batch: Vec<SpanData>) -> OTelSdkResult {
        for span in &mut batch {
            redact(&mut span.attributes, &self.policy);
            for event in &mut span.events.events {
                redact(&mut event.attributes, &self.policy);
            }
            for link in &mut span.links.links {
                redact(&mut link.attributes, &self.policy);
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
        self.inner.set_resource(&self.policy.resource(resource));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_keys_are_additive_case_insensitive_and_bounded() {
        let policy = Policy::new(Mode::Public, " ACCOUNT_NUMBER, private.id ").unwrap();
        let mut attributes = vec![
            KeyValue::new("customer_account_number", "custom-secret"),
            KeyValue::new("PRIVATE.ID", "custom-secret"),
            KeyValue::new("email", "builtin-secret"),
            KeyValue::new("outcome", "next"),
        ];
        redact(&mut attributes, &policy);
        assert!(
            attributes[..3]
                .iter()
                .all(|v| v.value.to_string() == "[REDACTED]")
        );
        assert_eq!(attributes[3].value.to_string(), "next");
        for invalid in [",", "key,", "a,,b", "secret=value", "key\n", "키", "*"] {
            assert_eq!(
                Policy::new(Mode::Off, invalid).unwrap_err(),
                "invalid additional redaction keys"
            );
        }
        assert!(Policy::new(Mode::Public, &"x".repeat(129)).is_err());
        assert!(Policy::new(Mode::Public, &vec!["x"; 65].join(",")).is_err());
        assert!(Policy::new(Mode::Public, &vec!["x"; 64].join(",")).is_ok());
        assert!(Policy::new(Mode::Public, "").is_ok());
    }
    #[test]
    fn attribute_policy_replaces_sensitive_values() {
        for mode in [Mode::Public, Mode::Strict] {
            let mut attributes = vec![
                KeyValue::new("customer_email", "fixture-secret"),
                KeyValue::new("API_KEY", "fixture-secret"),
            ];
            redact(&mut attributes, &Policy::new(mode, "").unwrap());
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
        redact(&mut attributes, &Policy::new(Mode::Public, "").unwrap());
        assert_eq!(attributes[0].value.to_string(), "next");
        redact(&mut attributes, &Policy::new(Mode::Off, "").unwrap());
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
                    policy: Policy::new(mode, "account_number").unwrap(),
                })
                .build();
            let tracer = provider.tracer("test");
            let mut span = tracer.start("fixture");
            span.set_attribute(KeyValue::new("customer_email", "fixture-secret"));
            span.set_attribute(KeyValue::new("ACCOUNT_NUMBER", "custom-secret"));
            span.add_event("event", vec![KeyValue::new("api_key", "fixture-secret")]);
            span.add_event(
                "custom-event",
                vec![KeyValue::new("account_number", "custom-secret")],
            );
            span.add_link(
                SpanContext::new(
                    TraceId::from(1),
                    SpanId::from(1),
                    TraceFlags::SAMPLED,
                    false,
                    TraceState::default(),
                ),
                vec![
                    KeyValue::new("token", "fixture-secret"),
                    KeyValue::new("account_number", "custom-secret"),
                ],
            );
            span.end();
            provider.shutdown().unwrap();
            let spans = capture.0.lock().unwrap();
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].events.events.len(), 2);
            assert_eq!(spans[0].links.links.len(), 1);
            let payload = format!("{spans:?}");
            assert_eq!(payload.contains("fixture-secret"), !protected);
            assert_eq!(payload.contains("custom-secret"), !protected);
            assert_eq!(payload.contains("[REDACTED]"), protected);
        }
    }
}
