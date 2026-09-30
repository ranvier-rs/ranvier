//! Finite real OTLP fixture; service identity remains otel-demo for collector assertions.
use async_trait::async_trait;
use opentelemetry::{
    metrics::MeterProvider,
    trace::{Span, Tracer, TracerProvider},
    KeyValue,
};
use ranvier_core::prelude::*;
use ranvier_observe::{
    otlp::{Protocol, Providers},
    redaction::Mode,
};
use ranvier_runtime::Axon;
use std::time::Duration;

#[derive(Clone)]
struct AddOne;
#[async_trait]
impl Transition<i32, i32> for AddOne {
    type Error = String;
    type Resources = ();
    async fn run(&self, value: i32, _: &(), _: &mut Bus) -> Outcome<i32, String> {
        Outcome::Next(value + 1)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let endpoint = std::env::var("RANVIER_OTLP_ENDPOINT")?;
    let protocol = match std::env::var("RANVIER_OTLP_PROTOCOL").as_deref() {
        Ok("grpc") => Protocol::Grpc,
        Ok("http/protobuf") => Protocol::HttpProtobuf,
        _ => return Err("explicit grpc or http/protobuf protocol required".into()),
    };
    let mode = match std::env::var("RANVIER_TELEMETRY_REDACT_MODE").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("public") => Mode::Public,
        Ok("strict") => Mode::Strict,
        Ok("off") => Mode::Off,
        _ => return Err("invalid redaction mode".into()),
    };
    let providers = Providers::build(
        "otel-demo",
        &endpoint,
        protocol,
        mode,
        Duration::from_secs(3),
    )?;
    let tracer = providers.traces.tracer("ranvier-otlp-smoke");
    let mut span = tracer.start("CalculationCircuit");
    span.set_attribute(KeyValue::new("customer_email", "demo.user@example.com"));
    span.set_attribute(KeyValue::new("api_key", "demo-api-key-123"));
    let outcome = Axon::<i32, i32, String>::new("CalculationCircuit")
        .then(AddOne)
        .then(AddOne)
        .execute(10, &(), &mut Bus::new())
        .await;
    let passed = matches!(outcome, Outcome::Next(12));
    let counter = providers
        .metrics
        .meter("ranvier-otlp-smoke")
        .u64_counter("ranvier_demo_runs_total")
        .build();
    counter.add(
        1,
        &[KeyValue::new(
            "outcome",
            if passed { "next" } else { "fault" },
        )],
    );
    span.end();
    let shutdown = tokio::task::spawn_blocking(move || providers.shutdown()).await?;
    shutdown.map_err(|_| "OTLP provider shutdown failed")?;
    if !passed {
        return Err("typed Axon outcome mismatch".into());
    }
    println!("OTLP_SMOKE_EXPORTED outcome=12");
    Ok(())
}
