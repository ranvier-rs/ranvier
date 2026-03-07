# Changelog

All notable changes to Ranvier are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.29.0] — 2026-03

### Summary

**Ranvier 0.29.0 — Level 4 "Production Ready" entry.**
Prometheus metrics endpoint, OTLP auto-export, AccessLogGuard, PostgresAuditSink, OpenAPI SecurityScheme + ProblemDetail, Docker/K8s deployment templates, operations guide (EN/KO), cross-crate integration tests.

### Added
- **Inspector Prometheus `/metrics` endpoint (M240):** Per-node invocations, errors, error rate, throughput, latency percentiles in Prometheus exposition format. BearerAuth protected.
- **`TelemetryConfig` in RanvierConfig (M240):** `[telemetry]` TOML section with `otlp_endpoint`, `otlp_protocol` (gRPC/HTTP), `service_name`, `sample_ratio`. Automatic TracerProvider initialization when endpoint is set; no-op otherwise.
- **`AccessLogGuard` standard node (M240):** Pass-through Guard Transition that reads `AccessLogRequest` from Bus, applies configurable path redaction, writes `AccessLogEntry` to Bus.
- **`PostgresAuditSink` (M241):** Feature-gated (`postgres`) sqlx-based audit event sink with hash chain integrity, migration SQL, `AuditSink` trait implementation (append/query/apply_retention). Connection pool configuration.
- **PII detection: 4 Korean patterns (M241):** 주민등록번호 (resident number), 사업자등록번호 (business number), 여권번호 (passport), 운전면허번호 (driver's license). Total: 13 PII categories.
- **OpenAPI `SecurityScheme` + `ProblemDetail` (M241):** `with_bearer_auth()` adds bearerAuth SecurityScheme. `with_problem_detail_errors()` adds RFC 7807 ProblemDetail schema and 400/404/500 error responses.
- **Docker multi-stage build template (M242):** 2-stage Dockerfile (rust:1.93 builder → debian:bookworm-slim runtime) with dependency caching, non-root user, HEALTHCHECK.
- **K8s deployment manifests (M242):** Deployment (readiness/liveness/startup probes, Prometheus annotations), Service (ClusterIP), ConfigMap (ranvier.toml), HPA (CPU/memory autoscaling).
- **Operations guide EN/KO (M242):** 8-section guide covering graceful shutdown, health checks, request ID, config loading, structured logging, Inspector observability, Prometheus scraping, OTLP export.
- **`production-operations-demo` example (M240):** Integrated demo combining config, health, metrics, access logging, and telemetry.
- **Cross-crate integration tests (M243):** 9 tests verifying audit×runtime×core, compliance×audit, std×runtime×core, openapi×http×core combinations.

### Changed
- **Compliance tests:** 0 → 25 tests covering Sensitive<T>, PiiDetector, ErasureSink, ClassificationLevel.
- **OpenAPI tests:** 4 → 12 tests covering SecurityScheme, ProblemDetail, multi-route consistency.
- **Inspector tests:** Added Prometheus text format, multi-circuit rendering, latency quantile, help/type line count tests.
- **Example count:** 59 → 60 examples (added production-operations-demo).

---

## [0.28.0] — 2026-03

### Summary

**Ranvier 0.28.0 — Documentation overhaul, example normalization, API quality, CLI template versioning.**
Macro-first Quickstart, comprehensive README rewrite, example learning DAG with Prerequisites/Next Steps, circuit factory inlining, production-path `unwrap()` → `expect()` conversion, and dynamic CLI template versioning.

### Added
- **`ranvier_core::VERSION` constant:** Compile-time crate version access via `env!("CARGO_PKG_VERSION")`. Used by CLI for dynamic template versioning.
- **Error Type Guide:** README table explaining when to use `String` (prototyping), custom enum (production), or `Never` (infallible transitions).
- **Bus Access Guide:** README table documenting `try_require()` (recommended), `read()` (optional), and `require()` (invariant) patterns.
- **Learning DAG:** `hello-world`, `reference-todo-api`, `reference-ecommerce-order` now include Prerequisites and Next Steps for guided learning progression.
- **Crate README expansion:** `ranvier-std` (Guard nodes table), `ranvier-audit` (Key Components), `ranvier-compliance` (Key Components + PII example), `ranvier-openapi` (Key Components + Swagger UI example).

### Changed
- **README.md rewritten:** Updated from v0.18.0/23 crates to v0.28.0/10 crates. Macro-first `#[transition]` Quickstart, "Under the Hood" manual impl section, 54 examples across 4 tiers, Built-in Production Features table.
- **`kit/README.md`:** Quickstart updated to `#[transition]` macro, version references 0.21→0.28, 10-crate architecture table.
- **`reference-todo-api`:** 6 single-transition circuit factory functions removed, inlined into `.route()` calls.
- **`reference-ecommerce-order`:** 3 simple factory functions inlined; `inventory_circuit()` (inline transition) and `order_pipeline_circuit()` (saga) retained.
- **`examples/README.md`:** Updated to v0.28.0, added 5 missing Tier B entries.
- **CLI template versioning (M238):** `ranvier new` Cargo.toml templates now derive crate version from `ranvier_core::VERSION` instead of hardcoded `"0.22"`. Template edition updated `2021` → `2024`, removed unnecessary `async-trait` dependency.

### Fixed
- **12 production-path `unwrap()` → `expect()`:** `runtime/axon.rs` (3: saga compensation registry lock, deserialization), `runtime/llm.rs` (1: prompt template guard), `http/ingress.rs` (6: Response builder), `http/body.rs` (1), `http/http3.rs` (1). All with descriptive panic messages.

---

## [0.27.0] — 2026-03

### Summary

**Ranvier 0.27.0 — Auth/Guard examples, ecosystem replacement coverage, CLI status page restoration.**
Completes the v0.21 crate consolidation (23→10) by providing replacement examples for all 13 removed crates. Adds Guard pipeline and IAM/JWT role-based access control examples, 6 ecosystem integration demos, Web Auth & Security manual pages, and restores CLI status page HTML generation.

### Added
- **Guard demo (M232):** `guard-demo` example showcasing 4 Guard nodes (CorsGuard, RateLimitGuard, SecurityHeadersGuard, IpFilterGuard) composed in an Axon `.then()` chain.
- **Auth JWT role demo (M232):** `auth-jwt-role-demo` example with `IamVerifier` implementation, `IamPolicy::RequireRole`, `Axon::with_iam()` boundary verification, and JWT token issuance.
- **Web Auth category (M233):** Auth & Security manual pages (EN/KO), `auth-security` learning path, example cards for guard-demo/auth-jwt-role/session-pattern.
- **Ecosystem replacement examples (M234):** `graphql-async-graphql-demo`, `grpc-tonic-demo`, `background-jobs-demo`, `distributed-lock-demo`, `db-sqlx-demo`, `typescript-codegen-demo` — covering all remaining gaps from removed crates.
- **CLI status page restoration (M235):** `ranvier status build` and `ranvier status from-schematic` now generate self-contained HTML status pages. Previously stubs since v0.21 crate consolidation.

### Changed
- **Example count:** 51 → 59 workspace examples.
- **CLI version:** 0.1.1 → 0.2.0.
- `multitenancy-demo` doc comments fixed: broken `session-demo` reference → `session-pattern`.

---

## [0.23.0] — 2026-03

### Summary

**Ranvier 0.23.0 — DX expansion: performance benchmarks, reference application, CLI scaffolding, WebSocket/SSE API testing.**
Quantitative performance baselines via criterion micro-benchmarks and Axum/Actix-web comparison servers, a complete multi-file Reference Todo API (CRUD + JWT auth + test collection), interactive CLI project scaffolding with 10 templates and dependency chooser, and WebSocket/SSE endpoint testing in the VSCode API Explorer.

### Added
- **Performance benchmarks (M218):** criterion micro-benchmarks for Axon latency, Bus operations, and Transition chain depth (1/3/10-step). Three Actix-web comparison servers alongside existing Axum servers for fair framework comparison. PowerShell benchmark runner script.
- **Reference Todo API (M219):** Complete multi-file example application with 6 transitions (login, CRUD), JWT auth module, typed error module, `Ranvier::http()` routing, and `.ranvier/collections/todo-crud.json` test collection with 12 requests and capture chaining.
- **CLI interactive scaffolding (M217):** `ranvier new` with `dialoguer`-based interactive template selection (10 templates), dependency chooser (DB: sqlx-postgres/sqlite, sea-orm; Auth: jwt; Observability: otlp, tracing), auto-generated `.ranvier/collections/` and `.env.example`.
- **WebSocket/SSE API testing (M216):** VSCode API Explorer with WebSocket bidirectional message panel (connect/disconnect, auto-reconnect, subprotocol headers, message filtering) and SSE stream panel (event type/data/id, filtering, Last-Event-ID reconnection).

### Changed
- **`bench` crate:** Added to workspace members. Removed stale `ranvier-auth`/`ranvier-guard` dependencies (consolidated in v0.21). Fixed `Infallible` → `ranvier_core::Never` across all scenario servers and benchmarks.

---

## [0.22.0] — 2026-03

### Summary

**Ranvier 0.22.0 — DX ergonomics, audit integrity, compliance depth, standard node library expansion.**
Derive macro for `ResourceRequirement`, Bus convenience API (`provide`/`require`), audit hash-chain integrity verification, compliance classification and PII detection, and 12 new standard library nodes including Bus-injectable guard transitions.

### Added
- **`#[derive(ResourceRequirement)]` (M212):** Proc-macro derive for the `ResourceRequirement` marker trait, eliminating manual `impl` boilerplate for Bus-injectable types.
- **`Bus::provide()` / `Bus::require()` / `Bus::try_require()` (M212):** Ergonomic convenience methods for dependency injection — `require()` panics with a helpful message naming the missing type.
- **`AuditChain` (M213):** Tamper-proof SHA-256 hash chain linking `AuditEvent` records via `prev_hash`. `verify()` detects modification and deletion.
- **`AuditQuery` builder (M213):** Fluent query API filtering by action, actor, target, and time range across any `AuditSink`.
- **`RetentionPolicy` (M213):** `max_age` / `max_count` retention with `ArchiveStrategy` (Delete or Archive callback). Implemented for `InMemoryAuditSink` and `FileAuditSink`.
- **`ClassificationLevel` (M214):** `Public` / `Internal` / `Confidential` / `Restricted` data classification enum for `Sensitive<T>`.
- **`EncryptionHook` trait (M214):** Pluggable encryption abstraction with `NoOpEncryption` and `XorEncryption` implementations.
- **`FieldNamePiiDetector` (M214):** Heuristic PII scanner detecting 9 categories (email, phone, SSN, credit card, name, address, DOB, IP, passport).
- **`ErasureRequest` / `ErasureSink` (M214):** GDPR right-to-erasure abstractions with `InMemoryErasureSink` implementation.
- **Validation nodes (M215):** `RequiredNode<T>`, `RangeValidator<T>`, `PatternValidator`, `SchemaValidator` — input validation as visible circuit transitions.
- **Transformation nodes (M215):** `MapNode`, `FilterTransformNode`, `FlattenNode`, `MergeNode` — data transformation as composable transitions.
- **Guard nodes (M215):** `CorsGuard`, `RateLimitGuard`, `SecurityHeadersGuard`, `IpFilterGuard` — Bus-injectable security guards replacing invisible middleware, visible in Schematic and Inspector Timeline.

### Changed
- **`Sensitive<T>` (M214):** Changed from tuple struct to named fields (`value`, `classification`). `Debug` output now shows classification level (e.g., `[REDACTED:Restricted]`).

---

## [0.21.0] — 2026-03

### Summary

**Ranvier 0.21.0 — Crate consolidation (23 → 10 crates), Hyper 1.0 native HTTP stack.**
Structural diet release: 13 thin-wrapper crates removed, paradigm types absorbed into core, tower/tower-http replaced with direct Hyper 1.0 usage. All removed crate functionality is preserved via external library direct usage with Transition-pattern examples.

### Changed
- **Crate consolidation:** 23 crates → 10 crates. Removed 13 crates that failed the paradigm test ("Does this crate operate on Transition/Outcome/Bus/Schematic?").
- **`ranvier-http` Hyper migration:** Removed `tower`, `tower-http`, `axum` dependencies. Now uses `hyper 1.0` directly with custom `BoxService` type-erasure, `flate2` compression, and `tokio::fs` static file serving.
- **`RanvierService` trait:** Changed from `tower::Service` to `hyper::service::Service` (`&self` call, no `poll_ready`).
- **10-crate publish DAG:** T0: core, macros → T1: audit, compliance, inspector, std → T2: runtime → T3: http → T4: openapi → T5: ranvier.

### Added
- **`ranvier-core::iam` module (M208):** `AuthContext` and `AuthScheme` absorbed from removed `ranvier-auth`. Bus-injectable authentication context.
- **`ranvier-core::tenant` module (M208):** `TenantId`, `TenantExtractor`, `TenantResolver`, `IsolationPolicy` absorbed from removed `ranvier-multitenancy`.
- **`matchit` routing (M210):** URL pattern matching via `matchit` crate (replaces tower/axum router internals).
- **`flate2` compression (M210):** Gzip response compression for static assets (replaces `tower_http::compression`).

### Removed
- **13 crates yanked from crates.io:** `ranvier-auth`, `ranvier-guard`, `ranvier-observe`, `ranvier-multitenancy`, `ranvier-graphql`, `ranvier-grpc`, `ranvier-synapse`, `ranvier-db`, `ranvier-redis`, `ranvier-session`, `ranvier-cluster`, `ranvier-job`, `ranvier-status`.
- **11 example demos removed:** Replaced by Transition-pattern alternatives and ecosystem integration examples.
- **Tower dependency:** `tower`, `tower-http`, `tower-layer`, `tower-service` removed from the dependency tree.

---

## [0.20.0] — 2026-03

### Summary

**Ranvier 0.20.0 — Inspector schema registry, request relay, and schema-aware macros.**
Introduces Inspector-side route discovery (`/api/v1/routes`), JSON Schema extraction (`/api/v1/routes/schema`, `/api/v1/routes/sample`), request relay API (`/api/v1/relay`), `#[transition(schema)]` macro attribute for compile-time schema generation, and `with_relay_target()` builder for Inspector relay configuration.

### Added
- **Schema Registry API:** `/api/v1/routes` endpoint enumerates all registered Axon routes with method, path, and transition metadata. `/api/v1/routes/schema` returns JSON Schema for route input/output types. `/api/v1/routes/sample` generates sample request payloads.
- **Request Relay API:** `/api/v1/relay` proxies requests through the Inspector to any registered route, capturing full circuit trace (timing, node transitions, outcomes) for debugging.
- **`#[transition(schema)]` macro attribute:** Generates `schema_for!(InputType)` under the `schemars` feature gate, enabling automatic JSON Schema extraction for transition input types.
- **`with_relay_target()` builder:** Configures the Inspector relay target URL, allowing Inspector-mediated request forwarding to the application server.

---

## [0.19.0] — 2026-03

### Summary

**Ranvier 0.19.0 — Example ergonomics, new demos, and VSCode DX.**
Introduces `Never` error type for infallible pipelines, 6 new example crates (47 total covering all 23 published crates), full crate README example link coverage, and VSCode code snippets + example commands.

### Added
- **`Never` type (M195):** `ranvier_core::Never` — serializable uninhabited error type replacing `std::convert::Infallible` for Axon pipelines. `InfallibleAxon` type alias updated.
- **New examples (M196–M197):** inspector-demo, audit-demo, compliance-demo, cluster-demo, status-demo, macros-demo. Total: 47 maintained examples.
- **Documentation sync (M198):** All 23 crate READMEs now have Examples sections. Manual docs, examples manifest, and web data fully synchronized.
- **VSCode snippets (M199):** 6 Rust snippets — `rvtransition`, `rv-transition`, `rvroute`, `rvaxon`, `rvbus`, `rvtest`.
- **VSCode example commands (M199):** `ranvier.loadExampleSchematic` and `ranvier.runExample` for browsing and running examples from the command palette.

---

## [0.18.0] — 2026-03

### Summary

**Ranvier 0.18.0 — crates.io release (23 crates).**
All 23 workspace crates published to crates.io at version 0.18.0. Inspector enrichment (v0.19 capability snapshot) landed: per-node metrics, payload capture, conditional breakpoints, and stall detection.

### Added
- **Inspector Metrics (M190–M191):** Sliding-window ring buffer collecting throughput, latency percentiles (p50/p95/p99), and error rate per node. REST query + WebSocket broadcast.
- **Payload Capture & DLQ (M192):** Configurable capture policy (off/hash/full) via `RANVIER_INSPECTOR_CAPTURE_PAYLOADS`. Dead letter queue inspection endpoints.
- **Conditional Breakpoints (M193):** JSON path `field op value` evaluator with CRUD REST API for setting breakpoints on node conditions.
- **Stall Detection (M193):** Threshold-based stall detector (`RANVIER_INSPECTOR_STALL_THRESHOLD_MS`, default 30s) with REST + WebSocket alerts.
- **Release Automation (M188):** `studio-tauri-release.yml` and `vscode-publish.yml` CI workflows.
- **E2E CI (M189):** `e2e-dogfooding.yml` workflow with 5 integration jobs.

---

## [0.17.0] — 2026-03

### Summary

**Ranvier 0.17.0 — VSCode Marketplace publish (v0.0.8), Studio stabilization, web manual guards.**

### Added
- **VSCode v0.0.8 (M184–M185):** Toolbox patterns, marketplace publish, limitation/boundary split rendering.
- **Web Manual Guards (M186–M187):** 14 verification scripts, manual drift detection, CI pilot workflow.
- **Studio Export Hardening (M35–M37):** Detached signature/checksum export, CI export verification smoke.
- **Capability Registry v0.17:** Limitation taxonomy refined (product_boundary vs implementation_gap).

### Changed
- CAPABILITY_REGISTRY.json: `cannot_do` items reduced from 11 to 3 (all `product_boundary`).

---

## [0.16.0] — 2026-03

### Summary

**Ranvier 0.16 — 1.0 Stable Release Preparation.**
API surface audit, CI hardening, community ecosystem documentation, and release preparation. All 23 crates version-synchronized at 0.16.0.

### Removed
- `static_gen::StaticNode` — use `StaticAxon` instead (deprecated since 0.14)
- `read_json_file`, `write_json_file` — internal utilities made `pub(crate)`

### Changed
- Minimum Supported Rust Version (MSRV): 1.93.0, Edition 2024
- Workspace-wide clippy lint configuration with `[lints] workspace = true` inheritance

### Added
- CONTRIBUTING.md with contribution guidelines
- PR template and issue templates (bug report, feature request)
- Plugin architecture design document
- Ecosystem integration guide (10 patterns)
- CI Architecture documentation
- Migration guide: 0.15 → 0.16

---

## [0.15.0] — 2026-02

### Summary

**Enterprise Production Readiness — Distributed execution, Saga patterns, DLQ, operational resilience.**

### Added
- **Distributed Execution (M170):** Redis-based distributed message bus, distributed locking, leader election, partitioned workflow execution
- **Enterprise DX Parity (M171):**
  - CLI: `ranvier state view/force-resume`, `ranvier deploy` (K8s/Docker scaffolding)
  - VSCode: Drag-and-drop circuit editor, step-through debugging
  - Studio: Multi-node fleet management, time-series diagnostics
- **Workflow Versioning (M172):** Schematic versioning, snapshot migration, event-sourcing replay, Studio "Active Intervention" panel
- **Operational Resilience (M173):** Dead-Letter Queues (DLQ), Saga compensation patterns, dynamic config reload, OIDC/OAuth2 IAM integration

---

## [0.14.0] — 2026-01

### Summary

**Security hardening, performance optimization, HTTP/3, GraphQL, enterprise features.**

### Added
- **Security Hardening (M161):** OWASP Top 10 compliance, DDoS protection (rate limiting, connection limits), input validation framework, SECURITY.md
- **Performance Optimization (M162):** Hot path profiling, memory allocation reduction, compilation time improvements
- **Advanced Observability (M163):** Custom metrics API (Counter, Gauge, Histogram), SLI/SLO tracking, span links, sampling, dashboard templates
- **HTTP/3 Support (M164):** QUIC transport, 0-RTT connection resumption, connection migration
- **GraphQL Ingress (M165):** `ranvier-graphql` crate with async-graphql, queries/mutations/subscriptions, DataLoader
- **Developer Tooling (M166):** VS Code Schematic visualization, `ranvier dev` hot reload, `ranvier trace` timeline replay
- **Enterprise Features (M167):** `ranvier-audit` (tamper-proof logging), `ranvier-multitenancy` (tenant isolation), `ranvier-compliance` (GDPR, HIPAA, SOC2), RBAC/ABAC

### Deprecated
- `static_gen::StaticNode` — use `StaticAxon` instead

---

## [0.13.0] — 2025-12

### Summary

**Performance benchmarks, SSE, Multipart, gRPC protocol extension.**

### Added
- **Cross-Framework Benchmarks (M157):** Performance comparison vs FastAPI, Express, Axum, Actix-web, Spring Boot
- **SSE Ingress (M158):** Server-Sent Events with `EventSource` bridge, keep-alive, retry, event IDs
- **Multipart Convenience (M159):** File upload extractor with size limits, streaming, automatic cleanup
- **gRPC Ingress (M160):** `ranvier-grpc` crate with tonic, unary/streaming RPCs, metadata bridge to Bus

---

## [0.12.0] — 2025-11

### Summary

**DX improvements — Router DSL, migration automation, CLI templates, OTel ops playbook.**

### Added
- **Router DSL (M151):** `HttpIngress::route_group(RouteGroup)` for large-scale route management
- **Migration Automation (M152):** `ranvier migrate --from 0.11 --to 0.12` with dry-run
- **CLI Templates (M153):** `ranvier new` templates (auth-service, crud-api, websocket-service, observability, event-driven)
- **OTel Ops Playbook (M154):** Vendor configs (Datadog, New Relic, Honeycomb, Jaeger, Tempo)
- **Adoption Resources (M155):** Quickstart guides, interactive tutorials, cookbook
- **Persistence Production Gate (M156):** Stabilized `PersistenceStore` API, ops runbook

---

## [0.11.0] — 2025-10

### Summary

**Enterprise readiness hardening — workflow persistence, OpenTelemetry interop, 1.0 governance.**

### Added
- **Workflow Persistence (M148):** `PersistenceStore` abstraction, PostgreSQL/Redis adapters, checkpoint/resume, compensation hooks (experimental)
- **OpenTelemetry Interop (M149):** OTLP exporter presets, attribute mapping, redaction controls (Public/Strict modes), Jaeger/Tempo/Datadog validation
- **1.0 Readiness RFC (M150):** API stability policy, support policy (MSRV, security patches), reliability gate, governance

---

## [0.10.0] — 2025-09

### Summary

**First stable release — API freeze, SemVer contract, enterprise adoption playbook.**

### Added
- Stabilized core Execution and Decision Engine APIs (Gate A)
- Typed fallback execution and error extraction in `ranvier-core`
- `ranvier-job` background job scheduling
- `ranvier-session` cache and session management backends
- Official extensions (`ranvier-auth`, `ranvier-guard`, `ranvier-openapi`) stabilized (Gate B)
- Graceful shutdown and lifecycle hooks
- Ecosystem reference examples (Gate C)
- **Enterprise Adoption Playbook (M146):** PoC scoring matrix, adoption decision framework
- **Macro Ergonomics Pack (M147):** Boilerplate reduction with explicit boundary preservation

### Changed
- API freeze enacted: no breaking changes in 0.10.x patches
- Promoted `v0.9.x` APIs to `v0.10.0`
- Transitioned static routing to decoupled `ranvier-http`

---

## [0.9.0] — 2025-08

### Summary

**API stabilization — performance baselines, background jobs, sessions.**

### Added
- **API Stabilization (M133):** Public API audit, `cargo-semver-checks` CI, MSRV 1.93 + Edition 2024
- **Performance Benchmarks (M134):** HTTP throughput, Axon latency, DB pipeline, memory profiling baselines
- **Documentation (M135):** Getting Started overhaul, migration guide (0.1→0.10), KO sync
- **ranvier-job (M136):** Background job scheduler with cron expressions + periodic tasks
- **ranvier-session (M137):** Session management with in-memory + Redis backends

---

## [0.7.0] — 2025-06

### Summary

**Web extension layer — auth, CORS, OpenAPI, WebSocket, observability.**

### Added
- **ranvier-auth:** JWT Bearer + API Key authentication, role-based authorization
- **ranvier-guard:** CORS layer, security headers (HSTS, CSP), rate limiting (token bucket)
- **ranvier-openapi:** Auto-generated OpenAPI 3.0 specs + embedded Swagger UI
- **Static File Serving:** `ServeDir` wrapper + SPA fallback
- **WebSocket:** HTTP → WebSocket upgrade, `EventSource`/`EventSink` bridge
- **ranvier-db:** Transaction isolation levels (`TxPgNode.with_isolation_level()`)
- **ranvier-observe:** W3C Trace Context, HTTP metrics, OTLP exporters
- **Schematic Diff (M144):** `ranvier schematic diff` CLI for structural change detection
- **Inspector Quick-View (M145):** Runtime circuit observation endpoints

---

## [0.5.0] — 2025-04

### Summary

**Production readiness — graceful shutdown, Tower middleware, test harness, Bus capability rules.**

### Added
- **Graceful Shutdown:** SIGTERM/SIGINT handling, connection draining, lifecycle hooks
- **Tower Middleware:** `.layer()` method for global + per-route middleware
- **Test Harness:** `TestApp`, `TestRequest`, `TestResponse` for integration tests
- **Health Checks:** Built-in `/health`, readiness/liveness probes
- **Request Validation:** `validator` crate integration, automatic 422 responses
- **Bus Capability Rules (M143):** Transition-scoped resource access control

---

## [0.2.0] — 2025-02

### Summary

**HTTP core — dynamic routing, request extractors, response mapping.**

### Added
- **Dynamic Routing:** Path parameters (`:id`, `*wildcard`)
- **Extractors:** `FromRequest` trait, `Json<T>`, `Query<T>`, `Path<T>`
- **Responses:** `IntoResponse` trait for Outcome→HTTP mapping
- **Convenience Methods:** `.get()`, `.post()`, `.put()`, `.delete()`
- **Error Handling:** Per-route error handler registration
- **Body Limits:** Automatic 400 Bad Request on parse errors

---

## [0.1.0] — 2025-01

### Summary

**Foundation — Decision Engine paradigm, core contracts, tooling infrastructure.**

### Added
- **Core Contracts:** `Transition<From, To>`, `Outcome<T, E>`, `Bus`, `Schematic`
- **Axon Execution Engine:** Linear and branching circuit execution
- **VS Code Plugin:** Circuit visualization, diagnostics, internationalization
- **Studio MVP:** Dual-surface architecture (remote server + desktop)
- **Inspector:** Quick-view baseline for runtime introspection
- **Documentation Hub:** Manual page structure, multilingual support (EN/KO)
