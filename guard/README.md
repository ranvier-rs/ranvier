# ranvier-guard

**Pipeline-first middleware for Ranvier** — 15 Guard Transition nodes that replace Tower middleware with visible, traceable, schematic-aware security and policy enforcement.

Guards are `Transition<T, T>` nodes: they read from the Bus, either pass through or fault with a typed rejection. Every Guard appears in the Schematic and Inspector Timeline.

## Guard Catalog

### Default Guards (12)

| Guard | Purpose | Rejection |
|-------|---------|-----------|
| `CorsGuard` | Origin validation + CORS response headers | 403 Forbidden |
| `AccessLogGuard` | Structured request logging with path redaction | pass-through |
| `SecurityHeadersGuard` | X-Frame-Options, CSP, HSTS, etc. | pass-through |
| `IpFilterGuard` | Allow-list / deny-list IP filtering | 403 Forbidden |
| `RateLimitGuard` | Token-bucket rate limiting per client | 429 Too Many Requests |
| `CompressionGuard` | Accept-Encoding negotiation (gzip/brotli) | pass-through |
| `RequestSizeLimitGuard` | Content-Length validation | 413 Payload Too Large |
| `RequestIdGuard` | X-Request-Id generation (UUID v4) or propagation | pass-through |
| `AuthGuard` | Bearer / API key / custom auth with timing-safe comparison | 401 Unauthorized |
| `ContentTypeGuard` | Content-Type media type validation | 415 Unsupported Media Type |
| `TimeoutGuard` | Pipeline execution deadline | 408 Request Timeout |
| `IdempotencyGuard` | Duplicate request prevention (TTL cache) | cache replay |

### Advanced Guards (3, feature: `advanced`)

| Guard | Purpose | Rejection |
|-------|---------|-----------|
| `DecompressionGuard` | Gzip request body decompression | 400 Bad Request |
| `ConditionalRequestGuard` | If-None-Match / If-Modified-Since (RFC 7232) | 304 Not Modified |
| `RedirectGuard` | 301/302 redirect rule matching | 301/302 Location |

## Usage

```rust
use ranvier_guard::prelude::*;
use ranvier_http::prelude::*;

Ranvier::http()
    .bind("127.0.0.1:3000")
    .guard(CorsGuard::<()>::new(CorsConfig::default()))
    .guard(CompressionGuard::<()>::new())
    .guard(RequestIdGuard::<()>::new())
    .guard(AuthGuard::<()>::bearer(vec!["my-token".into()]))
    .get("/api/hello", hello_circuit)
    .run(())
    .await
```

### Per-Route Guards

```rust
use ranvier_http::guards;

Ranvier::http()
    .guard(AuthGuard::<()>::bearer(vec!["token".into()]))
    .get("/api/public", public_circuit)
    .post_with_guards("/api/orders", order_circuit, guards![
        TimeoutGuard::<()>::secs_30(),
        ContentTypeGuard::<()>::json(),
        IdempotencyGuard::<()>::ttl_5min(),
    ])
    .run(())
    .await
```

## Features

- `default` — 12 Guards (no extra dependencies)
- `advanced` — adds 3 Tier 3 Guards (depends on `flate2`)

## License

MIT OR Apache-2.0
