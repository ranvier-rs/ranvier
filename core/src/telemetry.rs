use crate::prelude::*;
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
    From: Send + 'static + Debug, // Input must be Debug for tracing
    To: Send + 'static + Debug,   // Output must be Debug for tracing
{
    type Error = T::Error;

    async fn run(&self, input: From, bus: &mut Bus) -> anyhow::Result<Outcome<To, Self::Error>> {
        // 1. Start Span
        println!("[Trace] Start Span: '{}' | Input: {:?}", self.name, input);
        let start = std::time::Instant::now();

        // 2. Run Inner Transition
        let result = self.inner.run(input, bus).await;

        // 3. End Span
        let duration = start.elapsed();
        match &result {
            Ok(Outcome::Next(val)) => {
                println!(
                    "[Trace] End Span: '{}' | Duration: {:?} | Outcome: Next({:?})",
                    self.name, duration, val
                );
            }
            Ok(Outcome::Branch(id, _)) => {
                println!(
                    "[Trace] End Span: '{}' | Duration: {:?} | Outcome: Branch({})",
                    self.name, duration, id
                );
            }
            Ok(Outcome::Jump(id, _)) => {
                println!(
                    "[Trace] End Span: '{}' | Duration: {:?} | Outcome: Jump({})",
                    self.name, duration, id
                );
            }
            Ok(Outcome::Emit(event_type, _)) => {
                println!(
                    "[Trace] End Span: '{}' | Duration: {:?} | Outcome: Emit({})",
                    self.name, duration, event_type
                );
            }
            Ok(Outcome::Fault(_)) => {
                println!(
                    "[Trace] End Span: '{}' | Duration: {:?} | Outcome: Fault",
                    self.name, duration
                );
            }
            Err(e) => {
                println!(
                    "[Trace] End Span: '{}' | Duration: {:?} | Error: {:?}",
                    self.name, duration, e
                );
            }
        }

        result
    }
}
