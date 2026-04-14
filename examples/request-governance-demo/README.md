# Request Governance Demo

Cross-cutting concerns wiring example for Ranvier.

This example demonstrates how the following fit together in one service:
- JWT authentication
- role/policy enforcement
- audit logging
- SQLite persistence
- structured RFC 7807 error mapping
- request-level observability via access logging

This example is the reference surface for the following M378 concerns:

- explicit request header capture via `bus_injector`
- guard-backed request logging via `AccessLogGuard`
- policy failures rendered through explicit error mapping, not hidden middleware shortcuts
- audit persistence and approval flow visibility in one service

## Endpoints

- `POST /login`
- `POST /requests`
- `GET /requests/:id`
- `POST /requests/:id/approve`

## Guard, Error, and Observability Surface

The request path is intentionally visible:

1. `bus_injector` copies request headers into `RequestHeaders`
2. `AccessLogGuard` provides request-level observability
3. transitions decode Bearer tokens from `Bus` rather than from hidden globals
4. `GovernanceError` is converted into `ProblemDetail`
5. route registration uses `get_with_error()` / `post_with_error()` so the HTTP error surface stays explicit

This example does **not** expose OpenAPI. Its role is different from `admin-crud-demo`:

- `admin-crud-demo` is the OpenAPI/reference-doc surface
- `request-governance-demo` is the structured error + guard/observability surface

## Run

```bash
cargo run -p request-governance-demo
```

## Smoke Flow

1. Login as `alice` / `alice123` and create a request
2. Attempt approval as `alice` -> should fail with structured error
3. Login as `admin` / `admin123`
4. Approve the request successfully

Expected error-surface checks:

- missing or invalid token -> `401 Unauthorized`
- non-admin approval attempt -> `403 Forbidden`
- missing request id -> `404 Request Not Found`
- validation failure -> `400 Validation Error`

## Notes

- Uses in-memory SQLite for zero-infra local runs
- Approval route uses explicit error mapping instead of hidden middleware behavior
- Audit events are persisted into an `audit_events` table
- Access logging is attached as a Guard, not as an invisible external stack

## Test Seam / Resource Override

This example also serves as an M379 seam reference:

- `BIND_ADDR` changes only the listening address
- `JWT_SECRET` swaps token signing/verification without changing the route graph
- `AppState` carries the SQLite pool as an explicit process-local resource

Useful seam patterns:

1. issue user/admin tokens against the same route graph and verify policy outcomes
2. replace the SQLite handle in a test harness or forked example without changing transition contracts
3. verify `GovernanceError` -> `ProblemDetail` mapping independently from auth token generation

Boundary reminder:

- the in-memory SQLite database is a local demo backing store
- authoritative multi-instance state still belongs in an external system
- Ranvier intentionally does not provide a framework-owned ORM wrapper or mocking DSL here
