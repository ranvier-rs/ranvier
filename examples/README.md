# Ranvier Examples

**Updated:** 2026-03-07
**Workspace:** v0.28.0 — 10 crates, Hyper 1.0 native (no tower/tower-http)
**Purpose:** Keep examples aligned with the Typed Decision Engine direction:
1. Axon execution is explicit.
2. Schematic is analysis data, not an executable graph.
3. Protocol concerns stay in adapter layers.

---

## 1. Example Tiers

### Tier A: Canonical (guide-linked)

These are the first examples users should run.

1. `hello-world` — HTTP ingress baseline
2. `typed-state-tree` — typed state progression baseline
3. `basic-schematic` — schematic export + runtime baseline
4. `otel-concept` — minimal OpenTelemetry concept baseline

### Tier B: Supported (advanced/reference)

These are maintained and useful, but not the first onboarding path.

1. `flat-api-demo`
2. `routing-demo`
3. `routing-params-demo`
4. `session-pattern`
5. `std-lib-demo`
6. `static-build-demo`
7. `static-spa-demo`
8. `studio-demo`
9. `websocket-loop`
10. `websocket-ingress-demo`
11. `complex-schematic`
12. `synapse-demo`
13. `order-processing-demo`
14. `multitenancy-demo`
15. `multipart-upload-demo`
16. `sse-streaming-demo`
17. `testing-patterns`
18. `custom-error-types`
19. `retry-dlq-demo`
20. `state-persistence-demo`
21. `persistence-production-demo`
22. `otel-ops-demo`
23. `inspector-demo`
24. `openapi-demo`
25. `audit-demo`
26. `compliance-demo`
27. `macros-demo`
28. `bus-capability-demo`
29. `guard-demo`
30. `auth-jwt-role-demo`
31. `reference-todo-api`
32. `reference-ecommerce-order`
33. `reference-chat-server`
34. `production-config-demo`
35. `llm-content-moderation`

### Tier C: Ecosystem Integration

External library direct usage — no Ranvier wrapper crate needed.

1. `ecosystem-redis-demo`
2. `ecosystem-diesel-demo`
3. `ecosystem-seaorm-demo`
4. `ecosystem-nats-demo`
5. `ecosystem-meilisearch-demo`
6. `graphql-async-graphql-demo`
7. `grpc-tonic-demo`
8. `background-jobs-demo`
9. `distributed-lock-demo`
10. `db-sqlx-demo`
11. `typescript-codegen-demo`

### Tier D: Experimental (not authoritative for architecture)

Retained for exploration. May not represent the current recommended direction.

1. `experimental/fullstack-demo`
2. `experimental/replay-demo`
3. `experimental/state-tree-demo`
4. `experimental/persistence-recovery-demo`

---

## 2. Removed in v0.21.0

The following examples were removed because the crates they depended on were
consolidated or removed (23 → 10 crate consolidation):

- ~~`auth-jwt-role-demo`~~ → **restored in v0.27** using `ranvier_core::iam` + `Axon::with_iam()`
- ~~`guard-demo`~~ → **restored in v0.27** using `ranvier_std` Guard nodes
- `observe-http-demo` → use `otel-concept` or `otel-ops-demo` with external OTEL crates
- `otel-demo` → replaced by `otel-concept`
- ~~`graphql-service-demo`~~ → **restored in v0.27** as `graphql-async-graphql-demo`
- ~~`grpc-service-demo`~~ → **restored in v0.27** as `grpc-tonic-demo`
- ~~`db-example`~~ → **covered by** `ecosystem-diesel-demo`, `ecosystem-seaorm-demo`, `db-sqlx-demo`
- ~~`cluster-demo`~~ → **restored in v0.27** as `distributed-lock-demo`
- `status-demo` → implement as Transition node
- ~~`job-scheduler-demo`~~ → **restored in v0.27** as `background-jobs-demo`
- `session-demo` → use `session-pattern` for Transition-based sessions

---

## 3. Alignment Notes

1. When docs and code diverge, prefer Tier A examples first.
2. Tier D examples should not be used as public API/architecture references until promoted.
3. Promotion criteria:
   - Compiles on workspace baseline
   - Matches current Axon/Schematic boundary language
   - Includes a short run path and expected output
