# Ranvier Security Guide

This document describes Ranvier's security model, recommended practices for production deployments, and how to report vulnerabilities.

## Security Model Overview

Ranvier is a **Typed Decision Engine** — it orchestrates business logic through Transitions, Outcomes, and Schematics. Security enforcement is the application developer's responsibility, but Ranvier provides building blocks to make it straightforward:

| Layer | Mechanism | Crate |
|---|---|---|
| Authentication | `IamVerifier` trait, `AuthContext` | `ranvier-core` |
| Authorization | `BusAccessPolicy`, role-based `AuthContext` | `ranvier-core` |
| Audit | `AuditSink`, `AuditChain` (tamper-proof) | `ranvier-audit` |
| Data Protection | `Sensitive<T>`, `Redact` trait, PII detection | `ranvier-compliance` |
| HTTP Security | `CookieJar` (RFC 6265), error masking, RFC 7807 | `ranvier-http` |
| Observability | `Inspector` (Bearer auth, constant-time comparison) | `ranvier-inspector` |

## Authentication Patterns

### Transition-Based (Recommended)

Use `IamVerifier` to validate tokens inside a Transition:

```rust
use ranvier_core::iam::{IamVerifier, AuthContext};

struct AuthTransition;

#[async_trait]
impl Transition for AuthTransition {
    type Input = HttpRequest;
    type Output = AuthContext;
    type Error = AuthError;

    async fn execute(&self, input: Self::Input, bus: &Bus) -> Outcome<Self::Output, Self::Error> {
        let verifier = bus.require::<JwtVerifier>();
        let token = extract_bearer(&input);
        match verifier.verify(token).await {
            Ok(identity) => Outcome::Next(AuthContext::from(identity)),
            Err(e) => Outcome::Fault(AuthError::Unauthorized(e)),
        }
    }
}
```

### Hyper Service Integration

For applications using Tower/Hyper middleware, inject `AuthContext` into the Bus via a middleware layer. See `examples/auth-tower-integration` for a complete example.

## Secret Management Checklist

- **Never hardcode secrets** in source code. Use environment variables:
  ```bash
  JWT_SECRET=your-256-bit-secret cargo run
  ```
- Rotate secrets periodically (at least every 90 days for production).
- Use a secret manager (HashiCorp Vault, AWS Secrets Manager, etc.) in production.
- Set minimum key length: HS256 requires at least 256 bits (32 bytes).

## CORS Configuration

Ranvier does not enforce CORS at the framework level — this is the responsibility of the HTTP adapter layer.

For `HttpIngress`-based applications, add CORS headers in a middleware hook:

```rust
ingress.middleware(|req, res| {
    res.headers_mut().insert("Access-Control-Allow-Origin", "https://your-domain.com".parse().unwrap());
    res.headers_mut().insert("Access-Control-Allow-Methods", "GET, POST, OPTIONS".parse().unwrap());
});
```

For studio-server, set `RANVIER_CORS_ORIGINS` to a comma-separated list of allowed origins.

## Security Headers

For production HTTP responses, set these headers (example for Cloudflare Pages `_headers` file):

```
/*
  Strict-Transport-Security: max-age=31536000; includeSubDomains; preload
  X-Content-Type-Options: nosniff
  X-Frame-Options: DENY
  Referrer-Policy: strict-origin-when-cross-origin
  Permissions-Policy: camera=(), microphone=(), geolocation=()
```

## Sensitive Data Handling

Use `Sensitive<T>` to wrap PII/PHI data:

```rust
use ranvier_compliance::{Sensitive, ClassificationLevel};

let email = Sensitive::new("user@example.com".to_string());
// Debug output: [REDACTED:Restricted]
// Display output: [REDACTED]
// Release build serialization: "[REDACTED]"
// Debug build serialization: "user@example.com" (for development)
```

Use `expose()` only when explicit data access is required (e.g., sending to a verified internal service over TLS).

## PostgresAuditSink Configuration

Table names are validated against `[a-zA-Z_][a-zA-Z0-9_]{0,62}` at configuration time to prevent SQL injection:

```rust
let config = PostgresAuditConfig::new(pool)
    .table_name("audit_events")?;  // Returns Result — invalid names are rejected
```

## Bus Access Policies

Use `BusAccessPolicy` to restrict which resources a Transition can access:

```rust
bus.set_access_policy("PaymentTransition", Some(
    BusAccessPolicy::allow_only(vec![
        BusTypeRef::of::<PaymentGateway>(),
        BusTypeRef::of::<AuditLogger>(),
    ])
));
```

Policy violations are logged via `tracing::error!` and return `None` (no panic).

For explicit error handling, use `bus.get::<T>()` which returns `Result<&T, BusAccessError>`.

## Error Response Configuration

In release builds, `outcome_to_response()` returns a generic `"Internal server error"` message without exposing internal details. In debug builds, the full error is included for development convenience.

For custom error formatting, use `outcome_to_response_with_error()` or implement `IntoProblemDetail` for RFC 7807 responses.

## OWASP Top 10 Mapping

| # | OWASP Category | Ranvier Coverage |
|---|---|---|
| A01 | Broken Access Control | `BusAccessPolicy`, `AuthContext`, `IamVerifier` |
| A02 | Cryptographic Failures | `Sensitive<T>` redaction, constant-time token comparison |
| A03 | Injection | `PostgresAuditSink` table name validation, parameterized queries |
| A04 | Insecure Design | Schematic-first architecture, explicit Outcome types |
| A05 | Security Misconfiguration | Production error masking, configurable CORS |
| A06 | Vulnerable Components | Regular dependency audits recommended |
| A07 | Auth Failures | `IamVerifier` trait, JWT validation patterns in examples |
| A08 | Data Integrity Failures | `AuditChain` tamper-proof hash chain |
| A09 | Logging & Monitoring | `Inspector`, `AuditLogger`, `tracing` integration |
| A10 | SSRF | No HTTP client in core — application responsibility |

## Reporting Vulnerabilities

If you discover a security vulnerability in Ranvier:

1. **Do not** open a public GitHub issue.
2. Email **security@cellaxon.com** with:
   - Description of the vulnerability
   - Steps to reproduce
   - Affected versions
3. We will acknowledge receipt within 48 hours.
4. We aim to release a fix within 7 days for critical vulnerabilities.
