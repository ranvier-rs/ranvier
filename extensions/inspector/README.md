# Ranvier Inspector (`ranvier-inspector`)

Built-in Inspector REST + WebSocket server for runtime observability and debugging.

## Features

- **Schematic & Trace APIs**: Expose `/schematic`, `/trace/public`, `/trace/internal`, `/events` endpoints.
- **Schema Registry**: `/api/v1/routes` enumerates registered Axon routes. `/api/v1/routes/schema` returns JSON Schema for input/output types. `/api/v1/routes/sample` generates sample payloads via server-side faker.
- **Request Relay**: `/api/v1/relay` proxies requests through Inspector to any registered route, capturing full circuit trace (timing, transitions, outcomes). Configure with `with_relay_target()` on the Inspector builder.
- **Per-Node Metrics**: Sliding-window ring buffer collecting throughput, latency percentiles (p50/p95/p99), and error rate per node. Broadcast via REST and WebSocket.
- **Payload Capture & DLQ**: Configurable capture policy (off / hash / full) via `RANVIER_INSPECTOR_CAPTURE_PAYLOADS`. Dead letter queue inspection and management.
- **Conditional Breakpoints**: JSON path `field op value` evaluator with CRUD API for setting breakpoints on specific node conditions.
- **Stall Detection**: Threshold-based detection for nodes exceeding configured duration (`RANVIER_INSPECTOR_STALL_THRESHOLD_MS`, default 30000ms).
- **Auth Enforcement**: Optional role/tenant header checks (`RANVIER_AUTH_ENFORCE`, `RANVIER_AUTH_REQUIRE_TENANT_INTERNAL`).

## REST Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/schematic` | Export current circuit schematic |
| GET | `/trace/public` | Public trace projection |
| GET | `/trace/internal` | Internal trace projection |
| GET | `/events` | WebSocket event stream |
| GET | `/api/v1/routes` | List all registered Axon routes |
| GET | `/api/v1/routes/schema` | JSON Schema for route input/output types |
| GET | `/api/v1/routes/sample` | Generate sample request payload |
| POST | `/api/v1/relay` | Relay request through Inspector to target route |

## Usage

```rust
use ranvier_inspector::InspectorBuilder;

let inspector = InspectorBuilder::new()
    .with_relay_target("http://127.0.0.1:3000")
    .build();
```

The relay target points to the application server. Requests sent to `/api/v1/relay` are forwarded to the target, and the response includes the full circuit trace captured during execution.

## Examples

- [`inspector-demo`](../../examples/inspector-demo/) — Runtime observability server with metrics, payload capture, stall detection, and conditional breakpoints

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
