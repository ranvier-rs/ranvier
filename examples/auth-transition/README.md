# Auth Transition Example

A local JWT/role decision example. It does not start an HTTP server.

## Run

Configure `JWT_SECRET` explicitly in your shell, then run:

```sh
cargo run -p auth-transition
```

Blank or missing configuration fails; there is no fallback signing key. The
finite demo signs local test tokens and runs admin, non-admin, missing and invalid
cases. Token creation is demonstration-only, not a login or credential issuer.

## Actual contracts

- `AuthResources` carries the signing/verification key through typed transitions.
- `authenticate` clears prior identity, verifies JWT and explicitly inserts
  `AuthContext` into Bus. Returning a value does not automatically insert it.
- `authorize` requires the admin role; missing context returns Fault.
- `protected_handler` requires context and returns a local Response. Directly
  calling it with forged Bus context is not an authentication boundary.
- Each invocation uses a fresh Bus. The local ExecutionReport policy accepts only
  Unpersisted with complete cleanup; other reports request reconciliation.
- Wrong expected demo outcomes return errors instead of merely printing success.

Tests exercise actual signed JWTs, typed expiry, fixed invalid-token errors,
blank identity/configuration, invalid/missing tokens, non-admin rejection,
forged prior identity replacement, missing-context Faults and report rejection.

## Remaining release gates

Real HTTP token/header extraction, tenant/issuer/audience policy, key rotation,
revocation, rate limits, response mapping/privacy, native/customer and persisted
execution acceptance remain OPEN. Local token validation is not deployment proof.
Source: `src/main.rs` and `src/auth.rs`; schematic mode exports the actual chain.
