# Ranvier Examples

**Updated:** 2026-02-04  
**Purpose:** Keep examples aligned with the Typed Decision Engine direction:
1. Axon execution is explicit.
2. Schematic is analysis data, not an executable graph.
3. Protocol concerns stay in adapter layers.

---

## 1. Example Tiers

### Tier A: Canonical (guide-linked)

These are the first examples users should run.

1. `hello-world` - HTTP ingress baseline
2. `typed-state-tree` - typed state progression baseline
3. `basic-schematic` - schematic export + runtime baseline
4. `otel-demo` - minimal trace wiring baseline

### Tier B: Supported (advanced/reference)

These are maintained and useful, but not the first onboarding path.

1. `flat-api-demo`
2. `routing-demo`
3. `routing-params-demo`
4. `session-pattern`
5. `std-lib-demo`
6. `otel-concept`
7. `db-example`
8. `static-build-demo`
9. `studio-demo`
10. `websocket-loop`
11. `complex-schematic`
12. `synapse-demo`
13. `order-processing-demo`
14. `auth-jwt-role-demo`
15. `guard-demo`
16. `static-spa-demo`
17. `websocket-ingress-demo`
18. `multipart-upload-demo`
19. `sse-streaming-demo`
20. `grpc-service-demo`
21. `graphql-service-demo`
22. `testing-patterns`
23. `custom-error-types`
24. `retry-dlq-demo`
25. `state-persistence-demo`
26. `multitenancy-demo`
27. `session-demo`
28. `persistence-production-demo`
29. `job-scheduler-demo`
30. `observe-http-demo`
31. `otel-ops-demo`
32. `ecosystem-redis-demo`
33. `ecosystem-diesel-demo`
34. `ecosystem-seaorm-demo`
35. `ecosystem-nats-demo`
36. `ecosystem-meilisearch-demo`
37. `bus-capability-demo`
38. `inspector-demo`
39. `audit-demo`
40. `compliance-demo`
41. `cluster-demo`
42. `status-demo`
43. `macros-demo`

### Tier C: Experimental (not authoritative for architecture)

These are retained for exploration and may not represent the current recommended direction end-to-end.

1. `experimental/fullstack-demo`
2. `experimental/replay-demo`
3. `experimental/state-tree-demo`

---

## 2. Alignment Notes

1. When docs and code diverge, prefer Tier A examples first.
2. Tier C examples should not be used as public API/architecture references until promoted.
3. Promotion criteria:
   - Compiles on workspace baseline
   - Matches current Axon/Schematic boundary language
   - Includes a short run path and expected output

---

## 3. Next Cleanup Targets

1. Promote `order-processing-demo` from Tier B to Tier A after guide-level stabilization.
2. Remove unfinished/refactor-only branches from `routing-demo`.
3. `fullstack-demo`, `replay-demo`, `state-tree-demo` moved under `examples/experimental/*` and workspace paths were updated.
4. Remaining work: add a short guide section mapping `order-processing-demo` transitions to trace projection fields.
