# Ranvier — Typed Decision Engine for Rust

**Execution you can read. Structure you can trust.**

Ranvier is not a web framework. It is a **Typed Decision Engine** that keeps execution explicit,
structure inspectable, and boundaries clear. Your Rust logic becomes a circuit you can reason about,
diff, and validate.

---

**Latest: v0.18.0** (23 crates on crates.io)

- **Inspector Enrichment (v0.19)**: Per-node metrics (throughput, latency percentiles, error rate), payload capture & DLQ, conditional breakpoints, stall detection via Inspector REST + WebSocket.
- **Enterprise Production (v0.15–v0.16)**: Distributed execution, saga patterns, DLQ, MSRV 1.93.0 Edition 2024, API audit & CI hardening.
- **Security & Performance (v0.14)**: HTTP/3, GraphQL, gRPC adapters, security hardening.
- **Cross-Framework Benchmarks (v0.13)**: SSE, Multipart, gRPC, cross-framework comparison suite.
- **Workflow Persistence (v0.11–v0.12)**: Router DSL, OpenTelemetry interop, migration automation, CLI templates.
- **Stable Core (v0.10)**: API freeze, SemVer contract, enterprise adoption playbook.

---

**What Ranvier is**

1. **Axon**: explicit execution chain built from typed transitions.
2. **Schematic**: static structural artifact extracted from Axon. It never executes runtime logic.
3. **Outcome**: control-flow as data (`Next`, `Branch`, `Jump`, `Emit`, `Fault`).
4. **Ingress/Egress**: protocol adapters at the boundary (HTTP lives here, not in core).
5. **Bus**: typed resource container that stays explicit (no hidden injection).

---

**Quickstart**

```bash
cargo add ranvier
cargo add tokio --features full
cargo add anyhow
cargo add async-trait
```

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
    let hello = Axon::<(), (), anyhow::Error>::new("Hello")
        .then(Hello);

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
```

`Ranvier::http()` is an **Ingress Builder**, not a web server.

---

**Examples** — 47 runnable demos

```bash
cargo run -p hello-world          # HTTP ingress baseline
cargo run -p typed-state-tree     # Typed state progression
cargo run -p basic-schematic      # Schematic export + runtime
cargo run -p macros-demo          # #[transition] macro DX
cargo run -p inspector-demo       # Runtime observability server
cargo run -p status-demo          # Static status page generation
```

See `examples/README.md` for the full tier-classified list (47 examples).

---

**MSRV**

- Rust `1.93.0` or newer.

---

**Workspace Structure**

1. `core/` — protocol-agnostic contracts (`Transition`, `Outcome`, `Bus`, `Schematic`)
2. `runtime/` — Axon execution engine
3. `http/` — Ingress/Egress adapter boundary
4. `std/` — standard transitions and utilities
5. `macros/` — macro helpers
6. `extensions/` — optional ecosystem modules
7. `examples/` — runnable reference apps

---

**Boundary Rules (Non-Negotiable)**

1. Core stays protocol-agnostic.
2. Schematic is structural and non-executable.
3. Flat API convenience must not hide control flow.
4. No hidden middleware-style magic.

---

**Links**

- Website: https://ranvier.studio
- Docs: https://github.com/ranvier-rs/docs
