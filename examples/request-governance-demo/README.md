# Request Governance Demo

Cross-cutting concerns wiring example for Ranvier.

This example demonstrates how the following fit together in one service:
- JWT authentication
- role/policy enforcement
- audit logging
- SQLite persistence
- structured RFC 7807 error mapping
- request-level observability via access logging

## Endpoints

- `POST /login`
- `POST /requests`
- `GET /requests/:id`
- `POST /requests/:id/approve`

## Run

```bash
cargo run -p request-governance-demo
```

## Smoke Flow

1. Login as `alice` / `alice123` and create a request
2. Attempt approval as `alice` -> should fail with structured error
3. Login as `admin` / `admin123`
4. Approve the request successfully

## Notes

- Uses in-memory SQLite for zero-infra local runs
- Approval route uses explicit error mapping instead of hidden middleware behavior
- Audit events are persisted into an `audit_events` table
