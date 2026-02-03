//! # Telemetry: Observability Decorators
//!
//! This module provides decorators for adding observability to Transitions.

use crate::bus::Bus;
use crate::outcome::Outcome;
use crate::transition::Transition;
use async_trait::async_trait;
use std::fmt::Debug;

/// Represents the context of a Trace (e.g., Trace ID, Span ID).
/// In a real OTLP implementation, this would hold the actual SpanContext.
#[derive(Debug, Clone)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
}

impl TraceContext {
    pub fn new() -> Self {
        Self {
            trace_id: uuid::Uuid::new_v4().to_string(),
            span_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

/// A wrapper Transition that adds telemetry (tracing) to any inner Transition.
/// This demonstrates the "Decorator" pattern for observability.
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
    From: Send + 'static + Debug,
    To: Send + 'static + Debug,
{
    type Error = T::Error;
    type Resources = T::Resources;

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
            tracing::debug!(?input, "Entering node transition");
            let start = std::time::Instant::now();

            let result = self.inner.run(input, resources, bus).await;

            let duration = start.elapsed();
            match &result {
                Outcome::Next(val) => {
                    tracing::info!(?val, ?duration, "Transition completed: Next");
                }
                Outcome::Branch(id, _) => {
                    tracing::info!(?id, ?duration, "Transition completed: Branch");
                }
                Outcome::Jump(id, _) => {
                    tracing::info!(?id, ?duration, "Transition completed: Jump");
                }
                Outcome::Emit(event_type, _) => {
                    tracing::info!(?event_type, ?duration, "Transition completed: Emit");
                }
                Outcome::Fault(e) => {
                    tracing::error!(error = ?e, ?duration, "Transition failed: Fault");
                }
            }
            result
        }
        .instrument(span)
        .await
    }
}
