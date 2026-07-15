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

Default features include the HTTP ingress adapter, std nodes, and Guard.
To slim down dependencies:

```toml
ranvier = { version = "0.51.0", default-features = false }
```

You can enable features explicitly:

```toml
ranvier = { version = "0.51.0", features = ["http", "std"] }
```

## Crates (12 publishable product crates, v0.51.0)

| Tier | Crate | Purpose |
|------|-------|---------|
| T0 | `ranvier-core` | Kernel: Transition, Outcome, Bus, Schematic, iam, tenant |
| T0 | `ranvier-macros` | Proc macros: `#[transition]` attribute |
| T1 | `ranvier-guard` | Request and decision-boundary policy guards |
| T1 | `ranvier-audit` | Audit trail persistence |
| T1 | `ranvier-compliance` | Regulatory compliance checks |
| T1 | `ranvier-inspector` | Schema registry + relay API |
| T1 | `ranvier-std` | Standard library: Filter, Switch, Log, etc. |
| T2 | `ranvier-runtime` | Async Axon execution engine |
| T3 | `ranvier-http` | Hyper 1.0 native HTTP ingress adapter |
| T4 | `ranvier-openapi` | OpenAPI spec generation |
| T4 | `ranvier-test` | Axon and transition test support |
| T5 | `ranvier` | Facade crate (this crate) |

13 wrapper crates were removed in v0.28.0. Use external libraries directly
with Transition-pattern examples.

## Examples

- [`hello-world`](../examples/hello-world/) — workspace-native HTTP ingress baseline;
  external applications use the equivalent `ranvier` facade imports shown above

The facade-only compile contract at `tests/compile/facade-only` verifies that
`use ranvier::prelude::*` supplies the candidate transition, resource, Axon,
Outcome, Bus, native HTTP, and raw-service hybrid entry points without direct
Ranvier subcrate dependencies.

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
