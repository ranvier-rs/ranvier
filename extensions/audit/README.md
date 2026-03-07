# Ranvier Audit (`ranvier-audit`)

Audit trail recording for Ranvier transition executions and state changes.

## Key Components

| Component | Purpose |
|---|---|
| `AuditLogger` | Core logger that writes events to an `AuditSink` |
| `AuditEvent` | Structured event: actor, action, target, intent, metadata |
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

## Examples

- [`audit-demo`](../../examples/audit-demo/) — Tamper-evident audit logging with HMAC-signed file sink and Bus injection

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
