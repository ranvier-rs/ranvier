# Tracing Concept

This finite local example uses ConnectionBus and Traced transitions, then applies
an explicit ExecutionReport policy. Only Unpersisted with complete cleanup is
mapped to the local response. Other reports require reconciliation without retry.

Traced creates tracing spans/events with node/resource metadata, static Outcome
kind and elapsed duration. It does not log input/output payloads, error text,
branch/event names or jump identifiers. Node labels must be safe operational
metadata. Removing Debug bounds also permits payload types without a formatter.

Run `cargo run -p otel-concept`. Authentication here is simulated and the example
does not start an HTTP server. No subscriber/exporter/OTLP collector is installed
by this example, so it does not establish exported traces or remote ingestion.
The local TraceContext type holds simulated IDs; this is not propagation proof.

Core tests capture tracing output across Next/Branch/Jump/Emit/Fault and verify
outcome preservation without private value logging. Example tests execute main
and inject durability/cleanup report cases. Whole-system telemetry redaction,
subscriber/exporter lifecycle, collector authentication, sampling, panic/drop
and drain behavior, real distributed propagation, native/customer and production
observability acceptance remain OPEN.
