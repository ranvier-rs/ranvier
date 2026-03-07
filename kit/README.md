# ranvier

**Ranvier** is a Typed Decision Engine for Rust.
This crate is the facade entry point that re-exports the core, runtime, and HTTP ingress layers.

## Install

```bash
cargo add ranvier
```

## Quick Start

```rust
use ranvier::prelude::*;

#[transition]
async fn greet(_input: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    Outcome::Next("Hello, Ranvier!".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let hello = Axon::<(), (), String>::new("Hello").then(greet);

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
ranvier = { version = "0.27.0", default-features = false }
```

You can enable features explicitly:

```toml
ranvier = { version = "0.27.0", features = ["http", "std"] }
```

## Crates (10-crate architecture, v0.27.0)

| Tier | Crate | Purpose |
|------|-------|---------|
| T0 | `ranvier-core` | Kernel: Transition, Outcome, Bus, Schematic, iam, tenant |
| T0 | `ranvier-macros` | Proc macros: `#[transition]` attribute |
| T1 | `ranvier-audit` | Audit trail persistence |
| T1 | `ranvier-compliance` | Regulatory compliance checks |
| T1 | `ranvier-inspector` | Schema registry + relay API |
| T1 | `ranvier-std` | Standard library: Filter, Switch, Log, etc. |
| T2 | `ranvier-runtime` | Async Axon execution engine |
| T3 | `ranvier-http` | Hyper 1.0 native HTTP ingress adapter |
| T4 | `ranvier-openapi` | OpenAPI spec generation |
| T5 | `ranvier` | Facade crate (this crate) |

13 wrapper crates were removed in v0.27.0. Use external libraries directly
with Transition-pattern examples.

## Examples

- [`hello-world`](../examples/hello-world/) — HTTP ingress baseline (uses `ranvier` facade crate)

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
