# Admin CRUD Demo

Admin-style CRUD backend with JWT login, SQLite, pagination, search, and OpenAPI.

This example is the **bridge example** after the official track:

`Hello World` -> `Todo API` -> `Order Pipeline` -> `Admin CRUD Demo`

## Endpoints

Public:
- `POST /login`

Protected:
- `GET /departments`
- `GET /users`
- `POST /users`
- `GET /users/:id`
- `PUT /users/:id`
- `DELETE /users/:id`

Docs:
- `GET /openapi.json`
- `GET /docs`

## Operability Surface

This example is the reference surface for the following M378 concerns:

- request context capture via `bus_injector`
- explicit Bearer-token auth using `RequestHeaders` + `require_admin()`
- OpenAPI generation from the actual ingress surface
- Swagger UI publishing from the generated `/openapi.json`

The point is not to hide these concerns behind framework magic. The request path stays visible:

1. HTTP headers and path params are copied into `Bus`
2. protected transitions read `RequestHeaders` from `Bus`
3. `require_admin()` verifies the JWT explicitly before the CRUD query runs
4. the same ingress definition is used to build the OpenAPI document

## Request Context Flow

The request context path is intentionally explicit:

- `bus_injector` inserts lower-cased request headers into `RequestHeaders`
- `PathParams` are inserted into `Bus` when present
- protected routes call `require_admin()` rather than relying on hidden middleware
- route metadata and response schemas feed `OpenApiGenerator::from_ingress(&ingress)`

This makes the auth and documentation path inspectable in both code and public docs.

## Run

```bash
cargo run -p admin-crud-demo
```

Defaults:
- App URL: `http://127.0.0.1:3120`
- Login: `admin` / `admin123`
- JWT secret: `admin-crud-demo-secret` (override with `JWT_SECRET`)

## OpenAPI Parity Check

Minimal parity check:

1. Start the service with `cargo run -p admin-crud-demo`
2. `POST /login` with `admin` / `admin123`
3. call `GET /openapi.json` and verify protected routes are documented
4. open `GET /docs` and verify the same surface is exposed through Swagger UI

Example login request:

```bash
curl -X POST http://127.0.0.1:3120/login \
  -H "content-type: application/json" \
  -d '{"username":"admin","password":"admin123"}'
```

Use the returned token as:

```text
Authorization: Bearer <token>
```

## Test Seam / Resource Override

This example is also a useful M379 seam reference because the process-local resources are explicit:

- `BIND_ADDR` changes only the listening address
- `JWT_SECRET` swaps the signing key without changing the route graph
- `ADMIN_PASSWORD` swaps login credentials without changing transition wiring
- `AppState` carries the SQLite pool plus docs payloads as explicit process-local resources

What this example is good for:

1. smoke tests that issue a token and exercise protected routes
2. swapping auth/config inputs without changing the HTTP surface
3. replacing the SQLite handle in a test-only fork or harness

What this example is **not** trying to be:

- a framework-owned ORM abstraction
- a hidden DI container
- a cross-instance authoritative state model

The in-memory SQLite database is convenient for local runs, but it is not the authoritative multi-instance state model. For that, use an external database and keep the same `Resources` boundary.
