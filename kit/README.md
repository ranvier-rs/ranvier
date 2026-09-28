# ranvier

**Ranvier** is a Typed Decision Engine for Rust.
This crate is the facade entry point that re-exports the core, runtime, and HTTP ingress layers.

## Install

```bash
cargo add ranvier@0.51.0
cargo add tokio@1 --features macros,rt-multi-thread
```

## Quick Start (crates.io 0.51.0)

```rust
use ranvier::prelude::*;
use ranvier::runtime::ClosureTransition;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let hello = Axon::<(), (), String>::new("hello").then(ClosureTransition::new(
        "greet",
        |_input: (), _bus: &mut Bus| Outcome::next("Hello, Ranvier!".to_string()),
    ));

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await?;

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

The published `ranvier@0.51.0` facade does not expose a `schema` feature;
`ranvier-runtime` does. For registry macro schema metadata, use the explicit
`ranvier::macros::transition` import and declare these dependencies directly:

```toml
ranvier-core = "0.51.0"
ranvier-runtime = { version = "0.51.0", features = ["schema"] }
async-trait = "0.1"
schemars = { version = "1", features = ["derive"] }
serde_json = "1"
```

The 0.51.0 macro expansion refers to those crate names from the consumer crate.
This differs from the current root-gitlink source candidate, so a local facade
macro compile does not prove the registry dependency contract.

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
