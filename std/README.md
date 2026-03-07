# Ranvier Std (`ranvier-std`)

Standard transition nodes and utility helpers for Ranvier circuits.

## Guard Transition Nodes

Guard nodes are `Transition<T, T>` — they pass input through on success or return `Fault` on rejection.
Every guard is visible in the Schematic as a named node.

| Guard | Purpose |
|---|---|
| `CorsGuard` | Validates request origin against allowed origins |
| `RateLimitGuard` | Per-client token-bucket rate limiting |
| `SecurityHeadersGuard` | Injects HSTS, CSP, X-Frame-Options into Bus |
| `IpFilterGuard` | Allow-list / deny-list IP filtering |

```rust
use ranvier_std::prelude::*;

let pipeline = Axon::new("Guarded API")
    .then(CorsGuard::new(cors_config))
    .then(RateLimitGuard::new(100, 60_000))  // 100 req/min
    .then(IpFilterGuard::allow_list(["127.0.0.1"]))
    .then(business_logic);
```

## Examples

- [`std-lib-demo`](../examples/std-lib-demo/) — Standard library node usage (Filter, Switch, Math, String)
- [`guard-demo`](../examples/guard-demo/) — Guard pipeline with CORS, rate limit, security headers, IP filter

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
