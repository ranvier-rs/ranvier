# Ranvier Examples

**Updated:** 2026-07-16
**Workspace:** v0.51.0 release — 12 crates, Hyper 1.0 native (no tower/tower-http)
**Purpose:** Keep examples aligned with the Typed Decision Engine direction:
1. Axon execution is explicit.
2. Schematic is analysis data, not an executable graph.
3. Protocol concerns stay in adapter layers.

> **Security Notice:** These examples are for **educational purposes only**.
> Before deploying to production, review the [Security Guide](../SECURITY.md)
> and ensure all secrets are loaded from environment variables, not hardcoded.
> Authentication examples require: `JWT_SECRET=your-secret-here cargo run`

**Inventory:** 71 published examples + 4 experimental examples = 75 workspace example packages.

---

## 1. Example Support Tiers

`.ranvier-examples-manifest.json` is the source of truth for workspace example
metadata. Each example entry has:

- `tier`: web publication grouping (`core`, `lab`, or `repo`);
- `supportTier`: maintenance commitment (`canonical`, `supported`, `lab`, or
  `archive`);
- `owner`: the maintainer area responsible for keeping the example aligned;
- `ciGate`: developer, release, scheduled-lab, or excluded cadence;
- `manualLink`: user-facing instructions required for maintained examples;
- `supportRationale`: why the example earns ongoing maintenance capacity.

| Support tier | Count | CI obligation | Documentation obligation |
|---|---:|---|---|
| Canonical | 5 | Default developer `cargo check`, `cargo test`, and Clippy without external runtime services | First-run guide path and expected output |
| Supported | 12 | Release gate; tests run when `runtimeRequirements` is empty | Owner, runtime requirements, manual link, and maintenance rationale |
| Lab | 54 | Scheduled/on-demand lab gate; no routine release promise | Explicit runtime requirements and caveats |
| Archive | 4 | Excluded from release gates | Historical/exploratory only |

Canonical examples:

1. `hello-world` — HTTP ingress baseline
2. `typed-state-tree` — typed state progression baseline
3. `basic-schematic` — schematic export + runtime baseline
4. `otel-concept` — minimal OpenTelemetry concept baseline
5. `outcome-variants-demo` — Outcome variants baseline

Archive examples:

1. `experimental/fullstack-demo`
2. `experimental/replay-demo`
3. `experimental/state-tree-demo`
4. `experimental/persistence-recovery-demo`

The twelve Supported examples are the bounded official/bridge/reference set
plus contract coverage for Bus, typed HTTP, persistence, testing, and
retry/DLQ. Their exact identities and rationales live in the manifest. Other
published examples remain discoverable as Lab material without inflating the
routine release promise.

The higher-cost public surfaces `admin-crud-demo`,
`reference-fullstack-admin`, and `request-governance-demo` remain Supported;
their explicit owners and release-gate obligations prevent them from becoming
unowned showcase promises.

The portfolio cap is five Canonical and twelve Supported examples. A new
maintained example must prove a gap, name an owner and gate, provide a manual
link, and define how it will be retired or replace an existing example.

Routine Cargo commands use workspace `default-members`, which is verified as
exactly the 12 product crates plus five Canonical examples. The local release
bundle, release tags, and explicit pre-release dispatches run the Supported
lane. A weekly or explicit Lab lane checks all 54 Lab packages; it does not
claim that optional external services were exercised. Archive entries cannot
be selected by the gate runner. Every executable lane enforces its time budget
and records its owner, duration, fail-fast/no-retry policy, selected packages,
and command results as a 30-day CI artifact. The Supported Node project is
pinned to the workspace's Node 24 release baseline.

Supported, lab, and archive assignments are intentionally machine-owned in
`.ranvier-examples-manifest.json`. The compatibility catalog at
`examples/catalog.json` keeps the legacy A/B/C/D tier field for CLI and VS Code
consumers, but also mirrors `support_tier` and `owner`. Use:

```bash
node scripts/list_manifest_examples.mjs --verify-portfolio --verify-workspace-members
node scripts/list_manifest_examples.mjs --support-tiers canonical,supported
node scripts/list_manifest_examples.mjs --support-tiers canonical,supported --runtime none
node scripts/tiered_example_gate.mjs --lane developer --phase check
node scripts/tiered_example_gate.mjs --lane release --phase all
node scripts/tiered_example_gate.mjs --lane lab --phase all
```

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

1. When docs and code diverge, prefer Canonical examples first.
2. Archive examples should not be used as public API or architecture references
   until promoted.
3. Promotion criteria:
   - Compiles on workspace baseline
   - Matches current Axon/Schematic boundary language
   - Includes a short run path and expected output
   - Fits inside the maintained cap or replaces/de-scopes an existing entry
