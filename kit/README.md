# ranvier

**Ranvier** is a Typed Decision Engine for Rust.
This crate is the facade entry point that re-exports the core, runtime, and HTTP ingress layers.

## Install

```bash
cargo add ranvier
```

## Quick Start

```rust
use async_trait::async_trait;
use ranvier::prelude::*;

#[derive(Clone)]
struct Hello;

#[async_trait]
impl Transition<(), String> for Hello {
    type Error = anyhow::Error;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::Next("Hello, Ranvier!".to_string())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let hello = Axon::<(), (), anyhow::Error>::new("Hello").then(Hello);

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
```

## Notes

- `Ranvier::http()` is an **Ingress Builder**, not a web server.
- Core contracts stay protocol-agnostic. HTTP semantics live in the adapter layer.

## Features

Default features include the HTTP ingress adapter and std nodes.
To slim down dependencies:

```toml
ranvier = { version = "0.1.0", default-features = false }
```

You can enable features explicitly:

```toml
ranvier = { version = "0.1.0", features = ["http", "std"] }
```

## Crates

- `ranvier-core`
- `ranvier-runtime`
- `ranvier-http`
- `ranvier-std`
- `ranvier-cli`
