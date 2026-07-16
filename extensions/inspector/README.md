# Ranvier Inspector (`ranvier-inspector`)

Built-in Inspector REST + WebSocket server for runtime observability and debugging.

## Features

- **Schematic & Trace APIs**: Expose `/schematic`, `/trace/public`, `/trace/internal`, `/events` endpoints.
- **Schema Registry**: `/api/v1/routes` enumerates registered Axon routes. `/api/v1/routes/schema` returns JSON Schema for input/output types. `/api/v1/routes/sample` generates sample payloads via server-side faker.
- **Request Relay**: `/api/v1/relay` proxies requests through Inspector to any registered route, capturing full circuit trace (timing, transitions, outcomes). Configure with `with_relay_target()` on the Inspector builder.
- **Per-Node Metrics**: Sliding-window ring buffer collecting throughput, latency percentiles (p50/p95/p99), and error rate per node. Broadcast via REST and WebSocket.
- **Bounded Event Metadata & DLQ**: The event ring stores bounded, one-hour metadata records and DLQ inspection data. The `off` / `hash` / `full` payload policy surface remains Experimental; raw payload capture is not activated by the current tracing layer.
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
use ranvier_core::config::ResolvedRuntimeConfig;
use ranvier_core::runtime_policy::RuntimeProfile;
use ranvier_core::schematic::Schematic;
use ranvier_inspector::Inspector;
use std::net::{IpAddr, Ipv4Addr};

let runtime = ResolvedRuntimeConfig::load_for(RuntimeProfile::Production)?;
let inspector = Inspector::new(Schematic::new("checkout"), 9090)
    .with_runtime_profile(runtime.profile())
    .with_bind_address(IpAddr::V4(Ipv4Addr::LOCALHOST))
    .with_bearer_token_from_env()
    .validate(&runtime)?;

inspector.serve().await?;
```

The relay target points to the application server. Requests sent to `/api/v1/relay` are forwarded to the target, and the response includes the full circuit trace captured during execution.

## Production Policy

- The production-aware path consumes core's exact `development` / `production` profile. Development defaults to loopback; Production requires an explicit bind address.
- Production exposes public read-only routes and `/metrics`; internal, event, quick-view, debug/state, and relay routes are hidden.
- Production requires a bearer token. `Inspector::allow_unauthenticated()` remains a legacy compatibility flag and is not a typed acknowledgement; only an applicable, reviewed, expiring core acknowledgement can authorize the validated path.
- When a bearer token is configured, every Inspector route requires `Authorization: Bearer <token>`.
- Empty bearer tokens are treated as disabled.
- Development keeps permissive CORS for local browser tools; Production does not add permissive CORS headers.
- Active and completed trace collections each have non-zero count/TTL bounds; event metadata has a 500-record, one-hour bound.
- Relay requests are dev/internal only and are bounded by `RANVIER_INSPECTOR_RELAY_TIMEOUT_MS` and `RANVIER_INSPECTOR_RELAY_MAX_CONCURRENT`.

`with_mode[_from_env]` and direct `serve()` retain the 0.51.x compatibility
path during the migration window. New deployments should use the validated
path above so invalid/conflicting legacy modes and unsafe policy are aggregated
before listener bind or background-task creation.

## Examples

- [`inspector-demo`](../../examples/inspector-demo/) — Runtime observability server with metrics, payload capture, stall detection, and conditional breakpoints

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
