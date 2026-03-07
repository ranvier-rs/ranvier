# Ranvier — Typed Decision Engine for Rust

**Execution you can read. Structure you can trust.**

Ranvier is not a web framework. It is a **Typed Decision Engine** that keeps execution explicit,
structure inspectable, and boundaries clear. Your Rust logic becomes a circuit you can reason about,
diff, and validate.

---

**Latest: v0.28.0** — 10 crates on [crates.io](https://crates.io/crates/ranvier)

- **v0.28**: Documentation overhaul, example normalization, API quality (`unwrap`→`expect`), CLI dynamic template versioning, `ranvier_core::VERSION` constant
- **v0.27**: Guard Transition nodes, JWT auth, GraphQL/gRPC adapters, background jobs, distributed lock, DB patterns, TypeScript codegen, 8 new examples
- **v0.26**: CLI `ranvier merge` + `ranvier codegen`, VSCode Schematic Diff Viewer, GraphQL/gRPC Explorer, Environment Manager, `LlmTransition`, `Axon::parallel()` FanOut/FanIn, Inspector production (BearerAuth, TraceStore, AlertHook)
- **v0.21**: Crate consolidation 23 → 10 via Paradigm Test, Hyper 1.0 native (no Tower/Axum)

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

`Ranvier::http()` is an **Ingress Builder**, not a web server.

---

**Under the Hood**

The `#[transition]` macro expands to a full `Transition` trait implementation.
When you need custom resources or fine-grained control, implement the trait directly:

```rust
use async_trait::async_trait;
use ranvier::prelude::*;

#[derive(Clone)]
struct Greet;

#[async_trait]
impl Transition<(), String> for Greet {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _input: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::Next("Hello, Ranvier!".to_string())
    }
}
```

---

**Error Type Guide**

| Scenario | Recommended Type | Reason |
|---|---|---|
| Prototyping / demos | `String` | Simple, no extra dependencies |
| Production services | Custom `enum` with `#[derive(Debug)]` | Domain-specific error handling |
| Infallible transitions | `Never` | Compile-time guarantee of no errors |

---

**Bus Access Guide**

| Method | Returns | When to use |
|---|---|---|
| `bus.try_require::<T>()` | `Result<&T, BusError>` | Default choice — clear error message if missing |
| `bus.read::<T>()` | `Option<&T>` | Resource is optional (may not exist) |
| `bus.require::<T>()` | `&T` (panics if missing) | Invariant guaranteed by prior step (e.g., after `with_iam()`) |

---

**Examples** — 54 runnable demos across 4 tiers

```bash
# Tier A: Start here
cargo run -p hello-world          # HTTP ingress baseline
cargo run -p typed-state-tree     # Typed state progression
cargo run -p basic-schematic      # Schematic export + runtime
cargo run -p otel-concept         # OpenTelemetry concept baseline

# Tier B: Advanced patterns
cargo run -p macros-demo          # #[transition] macro before/after
cargo run -p guard-demo           # CorsGuard, RateLimitGuard, IpFilterGuard
cargo run -p auth-jwt-role-demo   # JWT + role-based access control
cargo run -p inspector-demo       # Runtime observability server

# Tier C: Ecosystem integration
cargo run -p graphql-async-graphql-demo  # async-graphql direct usage
cargo run -p grpc-tonic-demo             # tonic gRPC direct usage
cargo run -p db-sqlx-demo                # SQLx direct usage
```

See `examples/README.md` for the full tier-classified list.

---

**MSRV**

- Rust `1.93.0` or newer (Edition 2024).

---

**Workspace Structure** (10 crates)

1. `core/` — protocol-agnostic contracts (`Transition`, `Outcome`, `Bus`, `Schematic`)
2. `runtime/` — Axon execution engine, saga compensation, persistence
3. `http/` — Ingress/Egress adapter boundary (Hyper 1.0 native)
4. `std/` — standard transitions: Guard nodes, utilities
5. `macros/` — `#[transition]`, `#[derive(ResourceRequirement)]`
6. `kit/` — facade crate (re-exports all of the above as `ranvier`)
7. `extensions/inspector/` — runtime observability server
8. `extensions/audit/` — audit trail logging
9. `extensions/compliance/` — PII detection, data classification
10. `extensions/openapi/` — OpenAPI spec generation
11. `examples/` — 54 runnable reference apps

---

**Built-in Production Features**

| Feature | API | Status |
|---|---|---|
| Graceful Shutdown | `graceful_shutdown(timeout)` + `on_shutdown()` | Ready |
| Health Check | `health_endpoint()`, `readiness_liveness_default()` | Ready |
| Request ID | `request_id_layer()` — UUID v4, bidirectional header | Ready |
| Config Loading | `config(&RanvierConfig)` — 4-layer: defaults → TOML → profile → env | Ready |
| Guard Pipeline | `CorsGuard`, `RateLimitGuard`, `SecurityHeadersGuard`, `IpFilterGuard` | Ready |
| JWT Auth | `Axon::with_iam(policy, verifier)` — `IamPolicy::RequireRole` | Ready |
| Parallel Execution | `Axon::parallel()` — FanOut/FanIn with Bus isolation | Ready |
| Saga Compensation | `Axon::compensate(rollback_fn)` — LIFO rollback on failure | Ready |
| LLM Integration | `LlmTransition` — LLM-as-Transition pattern | Ready |
| Compression | gzip via flate2 | Ready |
| HTTP/2 | Hyper 1.0 native | Ready |
| Static Files | `serve_dir()` + `spa_fallback()` | Ready |
| Inspector | REST/WS metrics, BearerAuth, TraceStore, AlertHook | Ready |

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
- Crates.io: https://crates.io/crates/ranvier
- GitHub Release: https://github.com/ranvier-rs/ranvier/releases
