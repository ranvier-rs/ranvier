# Streaming Demo

Typed SSE reference example for Ranvier.

This example is the canonical M387 SSE reference because it exercises more of
the realtime request boundary than `sse-streaming-demo`:

- typed request body (`POST /api/chat/stream`)
- explicit ingress guards (`CorsGuard`, `AccessLogGuard`, `RequestIdGuard`)
- health/readiness/liveness endpoints
- graceful shutdown configuration

## Run

```sh
cargo run -p streaming-demo
```

Optional bind override:

```sh
STREAMING_DEMO_ADDR=127.0.0.1:3312 cargo run -p streaming-demo
```

## Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/chat/stream` | SSE stream of chat chunks |
| `POST` | `/api/chat` | non-streaming JSON response |
| `GET` | `/health` | health check |
| `GET` | `/ready` | readiness check |
| `GET` | `/live` | liveness check |

## Realtime Boundary Notes

1. The SSE route is request-boundary-first: the client sends a JSON request and
   receives a streaming response on the same HTTP boundary.
2. `AccessLogGuard` and `RequestIdGuard` keep request-level observability
   explicit rather than hiding it behind an external middleware stack.
3. This demo does not authenticate requests. If you need protected SSE routes,
   pair the streaming route with explicit auth guards and document the auth
   scheme in OpenAPI/runtime docs separately.

## Deployment Notes

1. Disable proxy buffering for SSE paths or the stream may appear to stall.
2. Tune idle timeouts in reverse proxies/load balancers so long-lived SSE
   responses are not cut off prematurely.
3. This demo emits chunks continuously, so it does not need a separate
   heartbeat. For idle SSE workloads, add heartbeat/keepalive events explicitly.
4. The process uses `graceful_shutdown(Duration::from_secs(5))`. Clients should
   tolerate disconnects and reconnect on shutdown/redeploy rather than expecting
   the server to preserve stream state across process restarts.
