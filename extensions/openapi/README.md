# Ranvier OpenAPI (`ranvier-openapi`)

OpenAPI 3.0 spec generation from Ranvier HTTP ingress routes.

## Key Components

| Component | Purpose |
|---|---|
| `OpenApiGenerator` | Builds an OpenAPI document from registered routes |
| `OpenApiDocument` | Serializable OpenAPI 3.0 spec |
| `swagger_ui_html()` | Generates Swagger UI HTML page for embedded docs |

## Usage

```rust
use ranvier_openapi::prelude::*;

let spec = OpenApiGenerator::new("My API", "1.0.0")
    .register_route("GET", "/users", &users_axon)
    .generate();

// Serve Swagger UI at /docs
Ranvier::http()
    .route("/docs", swagger_ui_html("/api/openapi.json"))
    .route("/api/openapi.json", spec)
```

## Examples

- [`openapi-demo`](../../examples/openapi-demo/) — OpenAPI spec generation from circuits

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
