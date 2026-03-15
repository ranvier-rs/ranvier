# Ranvier Runtime (`ranvier-runtime`)

> **The Engine:** Async execution and state management for Ranvier circuits.

## Purpose

`ranvier-runtime` provides the execution engine for Ranvier's typed decision pipelines:

- **Axon Execution**: Composable pipeline builder with `.then()` chaining
- **Resilience**: `then_with_retry()` and `then_with_timeout()` for production-grade fault tolerance
- **Persistence**: Durable checkpointing via `PersistenceStore` trait (InMemory, PostgreSQL, Redis)
- **Compensation**: Automatic rollback hooks on irrecoverable faults

## Axon Builder

```rust
use ranvier_runtime::{Axon, retry::RetryPolicy};
use ranvier_core::prelude::*;
use std::time::Duration;

let pipeline = Axon::<Input, Input, String>::new("OrderPipeline")
    .then(validate_order)
    .then_with_retry(charge_payment, RetryPolicy::exponential_default(3, 100))
    .then_with_timeout(
        notify_shipping,
        Duration::from_secs(5),
        || "Shipping notification timed out".to_string(),
    );
```

## Resilience Methods

| Method | Purpose |
|---|---|
| `then_with_retry(transition, policy)` | Retry on `Fault` with configurable backoff |
| `then_with_timeout(transition, duration, error_fn)` | Cancel execution if duration exceeded |

**RetryPolicy options:**
- `RetryPolicy::fixed(max_attempts, delay)` — constant delay between retries
- `RetryPolicy::exponential(max, base, multiplier, cap)` — exponential backoff
- `RetryPolicy::exponential_default(max, initial_ms)` — exponential with 2x multiplier

## Persistence Stores

| Adapter | Feature flag | Best for |
|---|---|---|
| `InMemoryPersistenceStore` | none (default) | tests, local dev |
| `PostgresPersistenceStore` | `persistence-postgres` | durable production storage |
| `RedisPersistenceStore` | `persistence-redis` | ephemeral/fast checkpoints |

```toml
# Enable PostgreSQL persistence
[dependencies]
ranvier-runtime = { version = "0.32", features = ["persistence-postgres"] }
```

```rust
use ranvier_runtime::{PostgresPersistenceStore, PersistenceHandle, PersistenceTraceId};

let pool = sqlx::postgres::PgPoolOptions::new()
    .max_connections(5)
    .connect("postgresql://user:pass@localhost/ranvier")
    .await?;

let store = PostgresPersistenceStore::new(pool);
store.ensure_schema().await?;

let handle = PersistenceHandle::from_store(store);
let mut bus = ranvier_core::ranvier_bus!(
    handle,
    PersistenceTraceId::new("order-123"),
);
```

## Examples

- [`hello-world`](../examples/hello-world/) — HTTP ingress baseline
- [`order-processing-demo`](../examples/order-processing-demo/) — Multi-step order processing pipeline
- [`state-persistence-demo`](../examples/state-persistence-demo/) — Persistence, recovery, and compensation
- [`resilience-patterns-demo`](../examples/resilience-patterns-demo/) — Retry and timeout patterns
- [`service-call-demo`](../examples/service-call-demo/) — HTTP client as Transition

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
