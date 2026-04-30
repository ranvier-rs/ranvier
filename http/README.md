# Ranvier HTTP (`ranvier-http`)

> **The Ingress:** Hyper 1.0 native HTTP adapter for Ranvier.

## 🎯 Purpose

`ranvier-http` bridges the gap between raw HTTP requests and Ranvier `Axon` circuits. It allows you to expose your business logic as a high-performance HTTP service with minimal boilerplate.

## 🔑 Key Components

- **`RanvierService`**: Implements `hyper::service::Service` for low-level adapter use.
- **`Ranvier` Builder**: The entry point for the "Flat API" (`Ranvier::http()`).
- **Input Converters**: Logic to map incoming `http::Request` to your circuit's `Input` type.
- **Static Asset APIs**: Explicit file-backed delivery via `serve_assets()` and `serve_spa_shell()`, with `serve_dir()` / `spa_fallback()` kept as compatibility shims.

## 🚀 Usage

```rust
use ranvier_core::{Bus, Outcome, Transition};
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

#[derive(Clone)]
struct Hello;

#[async_trait::async_trait]
impl Transition<(), String> for Hello {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::next("hello".to_string())
    }
}

Ranvier::http::<()>()
    .bind("127.0.0.1:3000")
    .get_json_out("/", Axon::simple::<String>("hello").then(Hello))
    .run(())
    .await?;
```

## Opt-In DTO Validation

Enable the `validation` feature when a typed JSON request should be validated
before it reaches a `Transition`:

```toml
[dependencies]
ranvier-http = { version = "0.44", features = ["validation"] }
validator = { version = "0.20", features = ["derive"] }
```

```rust,ignore
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Clone, Serialize, Deserialize, JsonSchema, Validate)]
struct CreateUser {
    #[validate(length(min = 3, max = 32))]
    #[schemars(length(min = 3, max = 32), regex(pattern = "^[a-z][a-z0-9_]+$"))]
    username: String,

    #[validate(email)]
    #[schemars(email)]
    email: String,
}

Ranvier::http::<AppState>()
    .post_validated_json_out("/users", create_user_circuit);
```

`post_validated_json_out`, `put_validated_json_out`, and
`patch_validated_json_out` return `422 Unprocessable Entity` with a stable
field-error JSON body when `validator::Validate` fails. Invalid JSON remains a
`400 Bad Request`. Existing `post_typed_json_out` routes remain schema-only and
do not require `Validate`, even when the feature is enabled.

For lower-level extractor code, use `ValidatedJson<T>` when validation should
run. `Json<T>` remains parse-only so enabling the `validation` feature does not
break existing extractor users through Cargo feature unification.

Runtime validation and OpenAPI metadata are related but not identical:
`#[validate(...)]` enforces server behavior, while `#[schemars(...)]` enriches
the generated schema consumed by OpenAPI, Orval, Zod, or similar tools. Custom
or cross-field validators may remain server-only when they cannot be expressed
as JSON Schema constraints.

## Examples

- [`hello-world`](../examples/hello-world/) — HTTP ingress baseline
- [`flat-api-demo`](../examples/flat-api-demo/) — Flat API routing
- [`routing-demo`](../examples/routing-demo/) — Route branching patterns
- [`routing-params-demo`](../examples/routing-params-demo/) — Route parameter extraction
- [`static-spa-demo`](../examples/static-spa-demo/) — explicit file-backed static assets + SPA shell
- [`large_api_demo`](examples/large_api_demo.rs) — grouped-route guard visibility + `route_descriptors()` proof

## Route / Guard Introspection

`route_descriptors()` now exports the effective guard stack for each route in
execution order. This keeps guard visibility explicit even when routes are
grouped or additional per-route guards are attached.

```rust,ignore
let ingress = Ranvier::http::<()>()
    .guard(AccessLogGuard::<()>::new())
    .group("/api", |g| {
        g.guard(RequestIdGuard::<()>::new())
            .get_json_out("/status", status_circuit)
    });

for route in ingress.route_descriptors() {
    println!("{} {}", route.method(), route.path_pattern());
    for guard in route.guard_descriptors() {
        println!("  - {} {:?}", guard.name(), guard.scope());
    }
}
```

## When To Use `group()`

Use `group()` when routes share a stable prefix and a shared guard/policy
context that would otherwise be repeated across many registrations.

Prefer plain route registration when:

- there are only one or two routes under the prefix
- the routes do not actually share guard semantics
- nesting would make the effective route/guard surface harder to explain than a
  flat list

If `route_descriptors()` becomes harder to read after grouping, the grouping is
probably too deep or too broad for Ranvier's explicitness goals.

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
