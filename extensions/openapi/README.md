# Ranvier OpenAPI (`ranvier-openapi`)

OpenAPI 3.0 generation helpers for `ranvier-http`.

## Key Components

| Component | Purpose |
|---|---|
| `OpenApiGenerator::from_ingress()` | Builds an OpenAPI document from `HttpIngress::route_descriptors()` |
| `OpenApiDocument` | Serializable OpenAPI 3.0 spec |
| `swagger_ui_html()` | Generates Swagger UI HTML for embedded docs |

## Usage

```rust,ignore
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_openapi::prelude::*;

let ingress = Ranvier::http::<()>()
    .get("/users/:id", get_user)
    .post_typed("/users", create_user)
    .health_endpoint("/healthz");

let openapi = OpenApiGenerator::from_ingress(&ingress)
    .title("Users API")
    .version("0.1.0")
    .with_bearer_auth()
    .with_problem_detail_errors()
    .summary(http::Method::GET, "/users/:id", "Get a user by id")
    .build_json();
```

## Automatic Inference Boundary

`ranvier-openapi` can infer the following directly from ingress descriptors:

- HTTP method and normalized path
- path parameters
- typed request body schema from `post_typed` / `put_typed` / `patch_typed`
- health/readiness/liveness routes exported by `HttpIngress`
- explicit `AuthGuard` hints carried by route descriptors when `with_bearer_auth()` is enabled

`ranvier-openapi` does **not** infer the following automatically:

- domain-specific error mappings that live outside OpenAPI-bearing examples
- transition output schemas unless the caller patches them
- security requirements for routes that do not expose explicit guard metadata
- API key/custom auth strategies carried by `AuthGuard` unless the caller adds
  explicit OpenAPI patches for them
- arbitrary response headers or policy behavior outside the descriptor surface

Use manual patches such as `summary()`, `json_request_schema()`,
`json_response_schema()`, `with_bearer_auth()`, and
`with_problem_detail_errors()` when the runtime surface needs more explicit
documentation than the ingress descriptors alone can provide.

## Examples

- [`openapi-demo`](../../examples/openapi-demo/) — primary generator/spec parity example
- [`admin-crud-demo`](../../examples/admin-crud-demo/) — authenticated OpenAPI/docs sanity reference

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
