# Ranvier Design Principles

**Version:** 0.43.0
**Updated:** 2026-04-03
**Applies to:** ranvier-core, ranvier-runtime, ranvier-http
**Category:** Architecture

---

## Introduction

This document records architectural decisions that shaped Ranvier's design. While [PHILOSOPHY.md](PHILOSOPHY.md) explains **why** Ranvier exists and what principles guide its use, this document explains **how we decided** on specific technical choices.

Each decision is recorded in Architecture Decision Record (ADR) format with context, alternatives, and consequences.

---

## Index

| ID | Title | Status | Date |
|----|-------|--------|------|
| DP-1 | [Paradigm Test: Crate Consolidation](#dp-1-paradigm-test-crate-consolidation) | Accepted | 2025-10 |
| DP-2 | [Tower Separation: Hyper 1.0 Native](#dp-2-tower-separation-hyper-10-native) | Accepted | 2025-09 |
| DP-3 | [Opinionated Core: Non-Negotiable Paradigm](#dp-3-opinionated-core-non-negotiable-paradigm) | Accepted | 2025-08 |
| DP-4 | [Static Assets: Edge Convenience, Not Core Identity](#dp-4-static-assets-edge-convenience-not-core-identity) | Accepted | 2026-04 |

---

## DP-1: Paradigm Test: Crate Consolidation

**Status:** Accepted
**Date:** 2025-10
**Version:** v0.21.0

### Context

Ranvier v0.20 had **23 crates**, each representing a thin abstraction over an ecosystem library:
- `ranvier-graphql` (wraps async-graphql)
- `ranvier-grpc` (wraps tonic)
- `ranvier-db` (wraps sqlx)
- `ranvier-redis`, `ranvier-session`, `ranvier-cluster`, `ranvier-job`, etc.
- 4 middleware crates: `ranvier-auth`, `ranvier-guard`, `ranvier-observe`, `ranvier-multitenancy`

**Problems**:
1. **Maintenance burden**: 23 crates to version, publish, document, and maintain
2. **Identity dilution**: Users were confused about which crates are "core Ranvier" vs. convenience wrappers
3. **False abstraction**: Most wrapper crates were 1-2 files thin and provided no real value
4. **Ecosystem duplication**: We were reimplementing what sqlx/tonic/redis already provide well

**Question**: Which crates are **essential to Ranvier's paradigm** (Transition/Outcome/Bus/Schematic), and which are just thin wrappers?

### Decision

We applied the **Paradigm Test**:

> "Does this crate fundamentally require Transition, Outcome, Bus, or Schematic to exist?"

**Test Results**:
- ✅ `ranvier-core` — defines Transition/Outcome/Bus/Schematic → **KEEP**
- ✅ `ranvier-runtime` — executes Axon (Transition pipeline) → **KEEP**
- ✅ `ranvier-http` — Ingress/Egress boundary for Transition → **KEEP**
- ✅ `ranvier-macros` — `#[transition]` macro → **KEEP**
- ✅ `ranvier-std` — standard Transitions (Guard nodes) → **KEEP**
- ✅ `ranvier-inspector`, `ranvier-audit`, `ranvier-compliance`, `ranvier-openapi` — extensions using paradigm → **KEEP**
- ✅ `kit/` (facade crate `ranvier`) — convenience re-export → **KEEP**

- ❌ `ranvier-graphql` — just re-exports async-graphql → **DELETE**
- ❌ `ranvier-grpc` — just re-exports tonic → **DELETE**
- ❌ `ranvier-db`, `ranvier-redis`, `ranvier-session`, `ranvier-cluster`, `ranvier-job`, `ranvier-status` → **DELETE**
- ❌ 4 middleware crates (auth/guard/observe/multitenancy) → **CONSOLIDATE** into core/std

**Result**: **23 crates → 10 crates**

### Consequences

**Positive**:
- ✅ **Clarity**: Users immediately know what's core Ranvier (10 crates) vs ecosystem tools
- ✅ **Maintenance**: 13 fewer crates to publish, version, document
- ✅ **Identity**: Ranvier is now clearly "a paradigm" not "wrappers for everything"
- ✅ **Ecosystem embrace**: Users directly use sqlx, tonic, redis — no Ranvier layer in between
- ✅ **Flexibility**: No forced abstractions — choose your DB/cache/queue library freely

**Negative**:
- ⚠️ **Migration burden**: v0.20 users must update imports (but we provide clear migration guide)
- ⚠️ **Perceived feature loss**: Some users may think we removed features (we actually only removed thin wrappers)

**Mitigation**:
- Comprehensive CHANGELOG with migration instructions
- Examples showing direct sqlx/tonic/redis usage in Transitions
- PHILOSOPHY.md explaining "Flexible Edges" principle

### Alternatives Considered

**Alternative A: Keep all 23 crates**
- ❌ Rejected: Maintenance burden too high, identity unclear

**Alternative B: Move wrappers to separate org (ranvier-contrib)**
- ⚠️ Considered: Still requires maintenance, but users might expect official support
- ❌ Rejected: Better to guide users to use ecosystem directly

**Alternative C: Deprecate but keep publishing old crates**
- ⚠️ Considered: Easier migration
- ❌ Rejected: Confuses new users, implies ongoing support

### References

- Consolidation PR: [ranvier#210](https://github.com/ranvier-rs/ranvier/pull/210)
- Migration guide: `CHANGELOG.md` v0.21.0 section
- Example updates: `examples/db-sqlx-demo`, `examples/graphql-async-graphql-demo`, `examples/grpc-tonic-demo`

---

## DP-2: Tower Separation: Hyper 1.0 Native

**Status:** Accepted
**Date:** 2025-09
**Version:** v0.21.0

### Context

Ranvier v0.20 used **Tower** (`tower`, `tower-http`) for HTTP handling:
- `ranvier-http` wrapped Tower's `Service` trait
- Middleware composition via Tower's `Layer` trait
- Dependency on `tower v0.4` + `tower-http v0.4`

**Problems**:
1. **Tower is middleware, not Ranvier's paradigm**: Tower's `Service<Request>` trait is fundamentally different from Ranvier's `Transition` trait. Mixing both confused users.
2. **Hidden complexity**: Tower's `Layer` ordering and `Service::poll_ready` semantics are complex. Ranvier's value proposition is **explicit execution**, yet Tower hides control flow.
3. **Hyper 1.0 compatibility**: Hyper 1.0 dropped `tower::Service` support, making Tower optional rather than required.
4. **Dependency bloat**: Pulling in Tower for basic HTTP was excessive when Hyper 1.0 provides native async fn support.

**Question**: Should Ranvier continue to require Tower, or embrace Hyper 1.0 natively?

### Decision

**Remove Tower dependency. Use Hyper 1.0 directly.**

**Implementation**:
- `ranvier-http` now uses `hyper::service::service_fn` directly
- `Ingress` trait maps HTTP requests → Axon input
- `Egress` trait maps Axon output → HTTP responses
- **No `tower::Service` implementation** — Ranvier is NOT a Tower middleware

**User migration path**:
```rust
// v0.20 (Tower-based)
let app = ServiceBuilder::new()
    .layer(CorsLayer::permissive())
    .service(ranvier_handler);

// v0.21+ (Hyper native, optional Tower)
// Option A: Pure Ranvier
let app = Ranvier::http()
    .route("/", axon)
    .run(resources).await?;

// Option B: Hybrid (user wraps Ranvier with Tower)
let ranvier_service = /* Ranvier handler wrapped as Tower Service */;
let app = ServiceBuilder::new()
    .layer(CorsLayer::permissive())
    .service(ranvier_service);
```

**Key insight**: Tower is now **optional integration**, not **required dependency**. Users who want Tower can wrap Ranvier handlers, but Ranvier itself doesn't depend on Tower.

### Consequences

**Positive**:
- ✅ **Simpler mental model**: Ranvier is Transition-based, not Service-based
- ✅ **Lighter dependencies**: No tower/tower-http unless user opts in
- ✅ **Hyper 1.0 native**: Leverage Hyper's async fn improvements
- ✅ **Clearer boundaries**: HTTP is at Ingress/Egress, not core paradigm
- ✅ **Flexible integration**: Users can wrap Ranvier with Tower/Axum/actix if needed

**Negative**:
- ⚠️ **Breaking change**: v0.20 users must update code (but migration is straightforward)
- ⚠️ **Loss of Tower ecosystem**: Can't use `tower-http` layers directly (but can integrate at boundary)

**Migration support**:
- Examples showing Tower integration (`examples/auth-tower-integration`)
- PHILOSOPHY.md Section 3.1 explaining ecosystem integration
- Comparison guide (`docs/guides/auth-comparison.md`)

### Alternatives Considered

**Alternative A: Keep Tower as core dependency**
- ❌ Rejected: Contradicts "Opinionated Core, Flexible Edges" — Tower is edge concern
- ❌ Rejected: Forces all users to learn Tower, even if they don't need middleware

**Alternative B: Make Tower optional feature**
- ⚠️ Considered: `ranvier-http/tower` feature
- ❌ Rejected: Still implies Tower is "blessed" integration, when any framework should work

**Alternative C: Provide both Tower and non-Tower APIs**
- ❌ Rejected: Doubles maintenance, confuses users about "the right way"

### References

- Tower removal PR: [ranvier#210](https://github.com/ranvier-rs/ranvier/pull/210)
- Hyper 1.0 migration: [ranvier#195](https://github.com/ranvier-rs/ranvier/pull/195)
- Integration examples: `examples/auth-tower-integration`, `examples/http-hyper-native`
- Philosophy: [PHILOSOPHY.md Section 3](PHILOSOPHY.md#3-why-flexible-edges)

---

## DP-3: Opinionated Core: Non-Negotiable Paradigm

**Status:** Accepted
**Date:** 2025-08
**Version:** v0.18.0+

### Context

Early Ranvier versions (v0.1–v0.17) experimented with various levels of flexibility:
- Optional `#[transition]` macro (users could implement `Transition` trait OR use free functions)
- Optional Bus (some pipelines used Bus, others used function arguments)
- Optional Schematic export (users could opt out of JSON generation)

**Problems**:
1. **Inconsistent codebases**: Some projects used Transition, others used free functions, fragmenting the ecosystem
2. **Tooling impossible**: The VSCode extension could not assume a Schematic existed, breaking Circuit view
3. **Lost identity**: "What makes Ranvier different from Axum/Actix?" had no clear answer
4. **Documentation burden**: Every guide had to explain "you can do X or Y or Z"

**Question**: Should Ranvier be flexible (support multiple patterns) or opinionated (enforce one paradigm)?

### Decision

**Ranvier's core paradigm (Transition/Outcome/Bus/Schematic) is NON-NEGOTIABLE.**

**Enforcement**:
1. **Transition is required**: All business logic must use `#[transition]` macro or implement `Transition` trait
2. **Outcome is required**: Transitions must return `Outcome<T, E>`, not `Result<T, E>`
3. **Bus is required**: Every Axon execution has a Bus (even if empty)
4. **Schematic is generated**: Every Axon produces a Schematic (JSON export is optional, but structure exists)

**Why opinionated**:
- **Identity**: Ranvier = "Schematic-first, visualizable framework" (unique value prop)
- **Learning curve**: One blessed path → faster onboarding (see [PHILOSOPHY.md Section 2.2](PHILOSOPHY.md#22-learning-curve-one-right-way-not-ten-ways))
- **Consistency**: All Ranvier codebases look alike → easier code review, onboarding
- **Tooling**: VSCode extension can assume Schematic exists → reliable Circuit view

**What remains flexible**:
- HTTP server choice (Hyper, actix, Axum) — Ingress/Egress
- Database choice (sqlx, diesel, sea-orm) — wrapped in Transitions
- Async runtime (tokio, async-std) — Ranvier is runtime-agnostic
- Deployment (Docker, K8s, Lambda) — Ranvier is just Rust code

See [PHILOSOPHY.md](PHILOSOPHY.md) for full "Opinionated Core, Flexible Edges" explanation.

### Consequences

**Positive**:
- ✅ **Clear identity**: Ranvier is the "Transition/Schematic framework" (not "Rust web framework #47")
- ✅ **Consistent ecosystem**: All examples, tutorials, projects follow same pattern
- ✅ **Reliable tooling**: VSCode extension, CLI tools, web dashboard all assume Schematic exists
- ✅ **Faster learning**: New users have one path to follow, not multiple competing patterns
- ✅ **Better debugging**: Schematic visualization works 100% of the time (not "opt-in feature")

**Negative**:
- ⚠️ **Less flexible**: Users who want `Result` or free functions must wrap in Transition
- ⚠️ **Higher barrier**: "Just use Axum" is simpler for trivial apps than learning the Transition paradigm
- ⚠️ **Migration from Actix/Axum**: Requires adopting the Transition paradigm; not a drop-in replacement

**Mitigation**:
- **Hybrid approach**: Users can embed Ranvier in existing apps (see [PHILOSOPHY.md Section 6.2 Path 3](PHILOSOPHY.md#path-3-actix-web-or-axum-integration-))
- **Clear docs**: PHILOSOPHY.md Decision Tree guides users on "when to use Ranvier"
- **Examples**: Show both "Pure Ranvier" and "Hybrid" patterns

### Alternatives Considered

**Alternative A: Make everything optional (maximum flexibility)**
- ❌ Rejected: Loses identity, tooling breaks, codebases inconsistent

**Alternative B: Support both Transition and free functions**
- ⚠️ Considered: Easier migration
- ❌ Rejected: Splits ecosystem, tooling can't assume structure

**Alternative C: Opinionated core + escape hatches**
- ⚠️ Considered: e.g., `Axon::raw_fn()` for non-Transition code
- ❌ Rejected: Every escape hatch is a consistency hole

**Alternative D (Current approach): Opinionated core + flexible edges**
- ✅ **Accepted**: Core is strict (Transition required), edges are flexible (any HTTP/DB library)

### References

- Paradigm enforcement: `ranvier-core` v0.18.0+
- Philosophy document: [PHILOSOPHY.md](PHILOSOPHY.md)
- Decision framework: [PHILOSOPHY.md Section 5](PHILOSOPHY.md#5-decision-framework)
- Examples: All 61 examples use Transition pattern

---

## DP-4: Static Assets: Edge Convenience, Not Core Identity

**Status:** Accepted
**Date:** 2026-04
**Version:** v0.43.0

### Context

`ranvier-http` now supports practical static asset features:
- `.serve_dir()` for directory mounts
- `.directory_index()` for index file resolution
- `.spa_fallback()` for client-side routing
- cache-control helpers, pre-compressed asset serving, and range requests

This created an architectural question: if most backend web frameworks serve
static files, should Ranvier evolve into a general-purpose static hosting
platform?

The risk is positioning drift. Static asset delivery is operationally important,
but it does not use Transition/Outcome/Bus/Schematic and does not benefit from
Schematic visibility. If over-emphasized, `Ranvier::http()` starts to look like
"another web server framework" instead of an ingress builder for Axon circuits.

### Decision

Keep static asset serving in `ranvier-http`, but define it as an **edge
convenience**, not a core identity.

Specifically:
1. Keep built-in support for co-serving API routes and a built frontend from one process.
2. Support pragmatic ingress features such as SPA fallback, cache control, pre-compressed assets, and range requests.
3. Do not position Ranvier as the primary solution for pure static hosting, CDN replacement, or general-purpose asset delivery.
4. Recommend dedicated servers or platforms such as nginx, Caddy, object storage, and CDNs for static-only or asset-heavy workloads.
5. Evaluate future work in this area by whether it improves application-boundary ergonomics, not by whether it makes Ranvier more "web-server-like."

### Consequences

**Positive**:
- ✅ **Clearer positioning**: Ranvier can support full-stack deployment patterns without claiming "static hosting framework" scope
- ✅ **Practical DX**: same-origin API + SPA deployment remains easy for demos, admin UIs, and reference apps
- ✅ **Architecture integrity**: Transition/Outcome/Bus/Schematic remain the center of the framework's identity
- ✅ **Better prioritization**: static asset work focuses on correctness and operability rather than platform sprawl

**Negative**:
- ⚠️ **Some users may expect more**: once `serve_dir()` exists, users may assume Ranvier should also own every hosting concern
- ⚠️ **Boundary explanation required**: docs must clearly distinguish "supported" from "recommended as primary workload"

**Mitigation**:
- Explain the boundary in PHILOSOPHY.md, cookbook docs, and use-case guides
- Keep hybrid deployment examples visible
- Recommend standard servers/CDNs explicitly for static-only workloads

### Alternatives Considered

**Alternative A: Remove static asset serving entirely**
- ❌ Rejected: same-process API + frontend delivery is a legitimate and common deployment need

**Alternative B: Make static hosting a first-class Ranvier platform goal**
- ❌ Rejected: pushes the framework toward general web-server scope and weakens its architectural identity

**Alternative C: Move static serving into a separate optional crate**
- ⚠️ Considered: preserves boundary clarity
- ❌ Rejected for now: current scope fits naturally inside `ranvier-http` as ingress functionality; extra crate split is unnecessary overhead

### References

- `docs/discussion/223_fullstack_sample_repo_strategy.md`
- `docs/discussion/249_when_to_use_ranvier_vs_axum.md`
- `docs/discussion/250_static_asset_serving_boundary.md`
- `docs/03_guides/cookbook_http_ingress.md`
- `docs/03_guides/use_cases.md`
- `examples/static-spa-demo`
- `examples/experimental/fullstack-demo`

---

## Contributing to This Document

When adding new design decisions:

1. **Use ADR format**: Context, Decision, Consequences, Alternatives
2. **Add to Index**: Update the table at the top
3. **Reference from code**: Add rustdoc comment linking to specific DP-X
4. **Cross-reference**: Link to PHILOSOPHY.md where relevant (PHILOSOPHY = why, DP = how)

**Template**:

```markdown
## DP-X: [Title]

**Status:** Proposed | Accepted | Deprecated
**Date:** YYYY-MM
**Version:** vX.Y.Z

### Context
[Why did this decision come up? What problem were we solving?]

### Decision
[What did we decide? Be specific.]

### Consequences
**Positive**: [Benefits]
**Negative**: [Costs/trade-offs]

### Alternatives Considered
[What other options did we evaluate? Why rejected?]

### References
[Links to PRs, issues, discussions, code]
```

---

## Related Documents

- [PHILOSOPHY.md](PHILOSOPHY.md) — Design philosophy (why Ranvier exists, when to use it)
- [README.md](README.md) — Project overview and quickstart
- [CHANGELOG.md](CHANGELOG.md) — Version history and migration guides
- [examples/README.md](examples/README.md) — 66 reference implementations

---

*This document is part of Ranvier v0.43.0. Last updated: 2026-04-03.*
