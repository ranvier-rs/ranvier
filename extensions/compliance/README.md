# Ranvier Compliance (`ranvier-compliance`)

Compliance helpers and policy enforcement primitives for Ranvier.

## Key Components

| Component | Purpose |
|---|---|
| `Sensitive<T>` | Wrapper that masks data in `Display`/`Debug` output |
| `ClassificationLevel` | 4-level data classification (Public, Internal, Confidential, Restricted) |
| `PiiDetector` | Pattern-based PII detection (email, phone, SSN, credit card, etc.) |
| `Redact` trait | Custom redaction strategy for domain types |
| `ErasureRequest` | GDPR right-to-erasure support primitives |

## Usage

```rust
use ranvier_compliance::*;

let email = Sensitive::new("user@example.com", ClassificationLevel::Confidential);
println!("{}", email); // prints "***REDACTED***"

let detector = PiiDetector::default();
let found = detector.detect("Call me at 010-1234-5678");
// found: [PiiMatch { category: Phone, ... }]
```

## Examples

- [`compliance-demo`](../../examples/compliance-demo/) — PII handling with Sensitive<T>, custom Redact, and PiiDetector

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
