//! # Telemetry: Observability Decorators
//!
//! This module provides decorators for adding observability to Transitions.

use crate::bus::Bus;
use crate::outcome::Outcome;
use crate::transition::Transition;
use async_trait::async_trait;

/// Represents the context of a Trace (e.g., Trace ID, Span ID).
/// In a real OTLP implementation, this would hold the actual SpanContext.
#[derive(Debug, Clone)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
}

impl Default for TraceContext {
    fn default() -> Self {
        Self {
            trace_id: uuid::Uuid::new_v4().to_string(),
            span_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl TraceContext {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A wrapper Transition that adds telemetry (tracing) to any inner Transition.
/// This demonstrates the "Decorator" pattern for observability.
/// Default logs contain static outcome kinds and durations, never input/output,
/// error text or control identifiers. Node labels must be safe operational metadata.
#[derive(Clone)]
pub struct Traced<T> {
    inner: T,
    name: String,
}

impl<T> Traced<T> {
    pub fn new(inner: T, name: &str) -> Self {
        Self {
            inner,
            name: name.to_string(),
        }
    }
}

#[async_trait]
impl<T, From, To> Transition<From, To> for Traced<T>
where
    T: Transition<From, To>,
    From: Send + 'static,
    To: Send + 'static,
{
    type Error = T::Error;
    type Resources = T::Resources;

    fn definition_source(&self) -> Option<crate::schematic::SourceLocation> {
        self.inner.definition_source()
    }

    async fn run(
        &self,
        input: From,
        resources: &Self::Resources,
        bus: &mut Bus,
    ) -> Outcome<To, Self::Error> {
        use tracing::{Instrument, info_span};

        let span = info_span!(
            "Node",
            ranvier.node = %self.name,
            ranvier.resource_type = %std::any::type_name::<Self::Resources>().split("::").last().unwrap_or("unknown")
        );

        async move {
            tracing::debug!("Entering node transition");
            let start = std::time::Instant::now();

            let result = self.inner.run(input, resources, bus).await;

            let duration = start.elapsed();
            // Payloads, error text and control identifiers may contain private data.
            // Default telemetry records only the static outcome kind and duration.
            match &result {
                Outcome::Next(_) => {
                    tracing::info!(outcome_kind = "Next", ?duration, "Transition completed")
                }
                Outcome::Branch(_, _) => {
                    tracing::info!(outcome_kind = "Branch", ?duration, "Transition completed")
                }
                Outcome::Jump(_, _) => {
                    tracing::info!(outcome_kind = "Jump", ?duration, "Transition completed")
                }
                Outcome::Emit(_, _) => {
                    tracing::info!(outcome_kind = "Emit", ?duration, "Transition completed")
                }
                Outcome::Fault(_) => {
                    tracing::error!(outcome_kind = "Fault", ?duration, "Transition failed")
                }
            }
            result
        }
        .instrument(span)
        .await
    }
}

/// Represents a manual intervention performed on an in-flight workflow.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InterventionEvent {
    /// A workflow was forcefully resumed from a stalled node.
    ForceResume {
        workflow_id: String,
        node_id: String,
        timestamp: chrono::DateTime<chrono::Utc>,
        operator: String,
        reason: Option<String>,
    },
    /// A workflow was manually failed.
    ForceFail {
        workflow_id: String,
        timestamp: chrono::DateTime<chrono::Utc>,
        operator: String,
        reason: Option<String>,
    },
    /// A queued manual intervention was applied during workflow resumption.
    ApplyIntervention {
        workflow_id: String,
        node_id: String,
        timestamp: chrono::DateTime<chrono::Utc>,
        operator: String,
        reason: Option<String>,
    },
    /// A systematic snapshot migration was applied to a workflow.
    SnapshotMigration {
        workflow_id: String,
        from_version: String,
        to_version: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },
}

/// A port for recording manual intervention audit logs.
#[async_trait]
pub trait AuditLogger: Send + Sync {
    /// Log an intervention event permanently and securely.
    async fn log_intervention(&self, event: InterventionEvent) -> Result<(), String>;
}

#[cfg(test)]
mod privacy_tests {
    use super::*;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    /// Serializes the tests that emit `Traced` events. A test that runs the same
    /// callsites in parallel without a subscriber can make tracing cache their
    /// interest as `never` while the capturing subscriber is being installed,
    /// which drops the first captured event (seen once on hosted CI).
    static TRACED_CALLSITES: Mutex<()> = Mutex::new(());

    fn lock_traced_callsites() -> std::sync::MutexGuard<'static, ()> {
        TRACED_CALLSITES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[derive(Clone)]
    struct Fixed(Outcome<String, String>);
    #[async_trait]
    impl Transition<String, String> for Fixed {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: String, _: &(), _: &mut Bus) -> Outcome<String, String> {
            self.0.clone()
        }
    }

    #[test]
    fn traced_logs_preserve_outcome_without_recording_private_values() {
        let _serial = lock_traced_callsites();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let output = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || Capture(output.clone()))
            .finish();
        let jump = uuid::Uuid::new_v4();
        let outcomes = [
            Outcome::Next("private-next".into()),
            Outcome::Branch(
                "private-branch".into(),
                Some(serde_json::json!({"value":"private-payload"})),
            ),
            Outcome::Jump(jump, Some(serde_json::json!({"value":"private-payload"}))),
            Outcome::Emit(
                "private-event".into(),
                Some(serde_json::json!({"value":"private-payload"})),
            ),
            Outcome::Fault("private-error".into()),
        ];
        let runtime = tokio::runtime::Runtime::new().unwrap();
        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                for expected in outcomes {
                    let actual = Traced::new(Fixed(expected.clone()), "safe-node")
                        .run("private-input".into(), &(), &mut Bus::new())
                        .await;
                    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
                }
            })
        });
        let bytes = captured.lock().unwrap();
        let logs = std::str::from_utf8(&bytes).unwrap();
        assert!(!logs.contains("private-"));
        assert!(!logs.contains(&jump.to_string()));
        for kind in ["Next", "Branch", "Jump", "Emit", "Fault"] {
            assert!(logs.contains(kind), "missing {kind}: {logs}");
        }
    }

    struct NoDebug(u32);
    #[derive(Clone)]
    struct Pass;
    #[async_trait]
    impl Transition<NoDebug, NoDebug> for Pass {
        type Error = String;
        type Resources = ();
        async fn run(&self, input: NoDebug, _: &(), _: &mut Bus) -> Outcome<NoDebug, String> {
            Outcome::Next(input)
        }
    }
    #[test]
    fn traced_does_not_require_payload_debug_formatting() {
        let _serial = lock_traced_callsites();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let outcome =
            runtime.block_on(Traced::new(Pass, "safe-node").run(NoDebug(7), &(), &mut Bus::new()));
        match outcome {
            Outcome::Next(value) => assert_eq!(value.0, 7),
            _ => panic!("unexpected outcome"),
        }
    }
}
