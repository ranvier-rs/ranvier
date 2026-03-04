# Changelog

All notable changes to Ranvier are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [1.0.0] — 2026-05

### Summary

**Ranvier 1.0 — Stable Release.**
The first production-stable release of Ranvier, marking the completion of the 0.x development cycle. All 23 crates are published with a strict SemVer 1.0 contract: no breaking changes in 1.x minor/patch releases.

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
- Migration guide: 0.15 → 1.0

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
