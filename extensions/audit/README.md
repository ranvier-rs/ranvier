# Ranvier Audit (`ranvier-audit`)

Audit trail recording for Ranvier transition executions and state changes.

## Key Components

| Component | Purpose |
|---|---|
| `AuditLogger` | Core logger that writes events to an `AuditSink` |
| `AuditLog<S>` | Composable `Transition<T, T>` node for explicit audit steps in an Axon chain |
| `AuditEvent` | Structured event: actor, action, target, intent, metadata |
| `AuditAction` | Standard action taxonomy for audit events (`Create`, `Update`, `Delete`, etc.) |
| `AuditActor` | Actor identity model (`User`, `System`, `Service`) extracted from `Bus` |
| `AuditSink` trait | Pluggable storage backend |
| `InMemoryAuditSink` | In-memory storage (testing) |
| `FileAuditSink` | HMAC-signed append-only file sink (tamper-evident) |
| `AuditQuery` | Query builder for filtering events by actor, action, time range |

## Usage

```rust
use ranvier_audit::*;

let sink = FileAuditSink::new("audit.log", hmac_key)?;
let logger = AuditLogger::new(Arc::new(sink));
bus.insert(logger); // Available to all downstream Transitions
```

### Explicit Axon-chain audit node

```rust
use ranvier_audit::{AuditAction, AuditActor, AuditLog, InMemoryAuditSink};
use std::sync::Arc;

let sink = Arc::new(InMemoryAuditSink::new());
let create_user = Axon::typed::<CreateUserInput, String>("user-create")
    .then(CreateUser)
    .then(AuditLog::new(sink.clone(), AuditAction::Create, "users"));

let mut bus = Bus::new();
bus.provide(AuditActor::User {
    id: "u-1".into(),
    name: "Alice".into(),
});
```

`AuditLog<S>` is intentionally a visible chain node instead of a hidden decorator.
It preserves the input value, appends an audit event, and keeps the audit path
visible in the pipeline structure.

## Examples

- [`audit-demo`](../../examples/audit-demo/) — Tamper-evident audit logging with HMAC-signed file sink and Bus injection

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
