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

## Examples

- [`hello-world`](../examples/hello-world/) — HTTP ingress baseline
- [`flat-api-demo`](../examples/flat-api-demo/) — Flat API routing
- [`routing-demo`](../examples/routing-demo/) — Route branching patterns
- [`routing-params-demo`](../examples/routing-params-demo/) — Route parameter extraction
- [`static-spa-demo`](../examples/static-spa-demo/) — explicit file-backed static assets + SPA shell

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
