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
- **Auth Enforcement**: Bearer token authentication plus optional role/tenant header checks (`RANVIER_AUTH_ENFORCE`, `RANVIER_AUTH_REQUIRE_TENANT_INTERNAL`).

## REST Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/schematic` | Export current circuit schematic |
| GET | `/trace/public` | Public trace projection |
| GET | `/healthz` | Inspector mode/auth/CORS/route policy summary |
| GET | `/trace/internal` | Internal trace projection |
| GET | `/events` | WebSocket event stream |
| GET | `/api/v1/routes` | List all registered Axon routes |
| GET | `/api/v1/routes/schema` | JSON Schema for route input/output types |
| GET | `/api/v1/routes/sample` | Generate sample request payload |
| POST | `/api/v1/relay` | Relay request through Inspector to target route |

## Usage

```rust
use ranvier_core::schematic::Schematic;
use ranvier_inspector::Inspector;

let inspector = Inspector::new(Schematic::new("checkout"), 9090)
    .with_mode_from_env()
    .with_bearer_token_from_env();

inspector.serve().await?;
```

The relay target points to the application server. Requests sent to `/api/v1/relay` are forwarded to the target, and the response includes the full circuit trace captured during execution.

## Production Policy

- `RANVIER_MODE=dev` exposes public, internal, event, quick-view, debug/state, and relay routes for local tooling.
- `RANVIER_MODE=prod` exposes public read-only routes and `/metrics`; internal, event, quick-view, debug/state, and relay routes are hidden.
- In `prod`, Inspector refuses to start without `RANVIER_INSPECTOR_TOKEN`, `Inspector::with_bearer_token(...)`, or an explicit `Inspector::allow_unauthenticated()` acknowledgement.
- When a bearer token is configured, every Inspector route requires `Authorization: Bearer <token>`.
- Empty bearer tokens are treated as disabled.
- Dev mode keeps permissive CORS for local browser tools; prod mode does not add permissive CORS headers by default.
- Relay requests are dev/internal only and are bounded by `RANVIER_INSPECTOR_RELAY_TIMEOUT_MS` and `RANVIER_INSPECTOR_RELAY_MAX_CONCURRENT`.

## Examples

- [`inspector-demo`](../../examples/inspector-demo/) — Runtime observability server with metrics, payload capture, stall detection, and conditional breakpoints

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
