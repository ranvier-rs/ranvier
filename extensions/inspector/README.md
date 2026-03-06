# Ranvier Inspector (`ranvier-inspector`)

Built-in Inspector REST + WebSocket server for runtime observability and debugging.

## Features

- **Schematic & Trace APIs**: Expose `/schematic`, `/trace/public`, `/trace/internal`, `/events` endpoints.
- **Per-Node Metrics**: Sliding-window ring buffer collecting throughput, latency percentiles (p50/p95/p99), and error rate per node. Broadcast via REST and WebSocket.
- **Payload Capture & DLQ**: Configurable capture policy (off / hash / full) via `RANVIER_INSPECTOR_CAPTURE_PAYLOADS`. Dead letter queue inspection and management.
- **Conditional Breakpoints**: JSON path `field op value` evaluator with CRUD API for setting breakpoints on specific node conditions.
- **Stall Detection**: Threshold-based detection for nodes exceeding configured duration (`RANVIER_INSPECTOR_STALL_THRESHOLD_MS`, default 30000ms).
- **Auth Enforcement**: Optional role/tenant header checks (`RANVIER_AUTH_ENFORCE`, `RANVIER_AUTH_REQUIRE_TENANT_INTERNAL`).

## Examples

- [`inspector-demo`](../../examples/inspector-demo/) — Runtime observability server with metrics, payload capture, stall detection, and conditional breakpoints

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
