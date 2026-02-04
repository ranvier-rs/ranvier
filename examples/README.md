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
3. `session-pattern`
4. `std-lib-demo`
5. `otel-concept`
6. `db-example`
7. `static-build-demo`
8. `studio-demo`
9. `websocket-loop`
10. `complex-schematic`
11. `synapse-demo`
12. `order-processing-demo`

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
