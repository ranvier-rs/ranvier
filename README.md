# Ranvier — Rust Web Backend for Complex Business Logic

**Every decision visible. Every branch auditable.**

Ranvier is a Rust web backend framework for services where business logic is too complex
for simple request-response handlers. Build multi-step workflows with typed Axon chains,
extract Schematics for CI diff, and keep every Outcome explicit — no hidden middleware.

---

**Latest: v0.44.0** — 12 crates on [crates.io](https://crates.io/crates/ranvier)

- **v0.44**: route/guard introspection, guard-aware OpenAPI metadata, canonical WS/SSE reference apps, static asset boundary defer-by-evidence
- **v0.43**: `Bus::get_cloned()`, `BusHttpExt`, `json_outcome()`, `get_json_out`/`post_typed_json_out`, `Json<T>` IntoResponse
- **v0.42**: `try_outcome!` macro, `Outcome::from_result`/`and_then`/`map_fault`, `CorsGuard::permissive()`, `post_json`
- **v0.41**: Inspector Lineage + Merkle Audit
- **v0.40**: CI/coverage workflows
- **v0.39**: Panic safety, Inspector auth, `RateLimitGuard` TTL
- **v0.38**: `#[streaming_transition]` macro, `StreamingAxon`, CLI pattern templates

---

## When Ranvier Is the Right Choice

| Scenario | Ranvier? | Why |
|----------|----------|-----|
| Multi-step payment saga with rollback | **Yes** | `then_compensated()` — LIFO compensation on failure |
| KYC/AML cascade screening | **Yes** | Sequential fail-fast filter pipeline with audit trail |
| AI agent pipeline (classify → tool → execute) | **Yes** | Typed state progression across stages |
| CI diff of execution paths before deploy | **Yes** | Schematic extraction — diff without running code |
| Simple CRUD REST API | **No** | Axum or actix-web is simpler for basic CRUD |
| High-throughput proxy/gateway | **No** | Axum + Tower has less per-request overhead |
| Existing Axum app + complex workflow | **Both** | [Use Ranvier inside Axum handlers](https://ranvier.studio/docs/integration-axum) |

---

## Core Concepts

1. **Axon**: Explicit execution chain built from typed transitions.
2. **Schematic**: Static structural artifact extracted from Axon. It never executes runtime logic.
3. **Outcome**: Control-flow as data (`Next`, `Branch`, `Jump`, `Emit`, `Fault`).
4. **Ingress/Egress**: Protocol adapters at the boundary (HTTP lives here, not in core).
5. **Bus**: Typed resource container that stays explicit (no hidden injection).

---

## Pattern Examples

```bash
# Saga: reserve → charge → ship, with reverse compensation on failure
cargo run -p saga-compensation

# Screening: sanctions → PEP → risk → documents, fail-fast
cargo run -p cascade-screening

# Pipeline: classify → select tool → execute → format, typed progression
cargo run -p llm-agent-pipeline

# IoT: sensor reading → normalize → detect anomaly → decide action
cargo run -p sensor-decision-loop
```

See all 68 published examples: [Examples Explorer](https://ranvier.studio/docs/examples-interactive)

---

## Quickstart

```bash
cargo add ranvier
cargo add tokio --features full
cargo add anyhow
```

```rust
use ranvier::prelude::*;

#[transition]
async fn greet(_input: (), _resources: &(), _bus: &mut Bus) -> Outcome<String, String> {
    Outcome::Next("Hello, Ranvier!".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let hello = Axon::<(), (), String>::new("Hello")
        .then(greet);

    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
```

Or scaffold with a pattern template:

```bash
cargo install ranvier-cli
ranvier new my-orders --template saga        # Saga compensation
ranvier new my-kyc    --template screening   # Cascade screening
ranvier new my-chat   --template pipeline    # Multi-step pipeline
```

---

## Using with Axum

Axum serves HTTP, Ranvier processes complex logic:

```rust
use axum::{Router, Json, routing::post};
use ranvier_runtime::Axon;
use ranvier_core::prelude::*;

async fn handle_order(Json(req): Json<OrderRequest>) -> Json<serde_json::Value> {
    let pipeline = Axon::typed::<OrderRequest, String>("order-saga")
        .with_saga_policy(SagaPolicy::Enabled)
        .then(validate_order)
        .then_compensated(reserve_inventory, release_inventory)
        .then_compensated(charge_payment, refund_payment)
        .then(complete_order);

    let mut bus = Bus::new();
    match pipeline.execute(req, &(), &mut bus).await {
        Outcome::Next(result) => Json(result),
        Outcome::Fault(err) => Json(serde_json::json!({"error": err})),
        _ => Json(serde_json::json!({"error": "unexpected"})),
    }
}

let app = Router::new().route("/api/orders", post(handle_order));
```

Read the full guide: [Axum Integration](https://ranvier.studio/docs/integration-axum) | [Ranvier vs Axum](https://ranvier.studio/docs/ranvier-vs-axum)

---

## Philosophy

**Opinionated Core, Flexible Edges**

- **Opinionated Core**: Ranvier enforces Transition/Outcome/Bus/Schematic for internal architecture. This is what makes Ranvier, Ranvier.
- **Flexible Edges**: At boundaries, use any Rust tool — Tower, Axum, sqlx, diesel, redis. Integrate with the ecosystem you already know.

Read the full philosophy: [PHILOSOPHY.md](docs/PHILOSOPHY.md)

---

## Workspace Structure (12 crates)

1. `core/` — protocol-agnostic contracts (Transition, Outcome, Bus, Schematic)
2. `runtime/` — Axon execution engine, saga compensation, persistence
3. `http/` — Ingress/Egress adapter boundary (Hyper 1.0 native)
4. `std/` — standard transitions: utilities
5. `guard/` — 15 Guard nodes: pipeline-first middleware (replaces Tower)
6. `macros/` — `#[transition]`, `#[streaming_transition]`, `#[derive(ResourceRequirement)]`
7. `testing/` — TestBus, TestAxon, assertion macros
8. `kit/` — facade crate (re-exports all as `ranvier`)
9. `extensions/inspector/` — runtime observability server
10. `extensions/audit/` — audit trail logging
11. `extensions/compliance/` — PII detection, data classification
12. `extensions/openapi/` — OpenAPI spec generation
13. `examples/` — 68 published + 4 experimental reference apps

---

**MSRV**: Rust `1.93.0` or newer (Edition 2024)

---

**Links**

- Website: https://ranvier.studio
- Docs: https://ranvier.studio/docs
- Crates.io: https://crates.io/crates/ranvier
- Examples: https://ranvier.studio/docs/examples-interactive
- GitHub Release: https://github.com/ranvier-rs/ranvier/releases
