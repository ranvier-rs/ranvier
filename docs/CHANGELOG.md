# Changelog

All notable changes to Ranvier are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.44.0] — 2026-04

### Summary

**Ranvier 0.44.0 — Inspectable HTTP Surface Sprint.**
Makes HTTP route/guard intent more inspectable, aligns OpenAPI generation with runtime guard metadata, promotes canonical realtime reference examples for WebSocket/SSE operability, and closes the static-asset follow-up by evidence instead of expanding the surface prematurely.

### Added
- **Route/guard introspection (ranvier-http, M385):** `HttpRouteDescriptor` now carries effective guard metadata in execution order via `HttpGuardDescriptor` / `HttpGuardScope`, including global, group, and per-route scope plus optional security-scheme hints.
- **Guard-aware OpenAPI metadata (ranvier-openapi, M386):** OpenAPI operations now emit `x-ranvier.guards` metadata and automatically attach `security` requirements only when route descriptors explicitly advertise a supported scheme hint such as `bearerAuth`.
- **Canonical realtime release smoke (examples + scripts, M387):** Added board-owned realtime smoke coverage using `reference-chat-server` for WebSocket and `streaming-demo` for SSE, with reproducible operability evidence.

### Changed
- **OpenAPI parity examples (M386):** `openapi-demo` now acts as the primary generator/spec reference, `admin-crud-demo` remains the authenticated docs sanity check, and `request-governance-demo` is documented as the runtime `ProblemDetail` reference rather than an OpenAPI surface.
- **Realtime reference examples (M387):** `reference-chat-server` request wiring and `streaming-demo` health/readiness/graceful-shutdown surfaces were hardened so the examples match the recommended production boundary patterns.
- **Static asset boundary decision (M388):** Kept file-backed static delivery as the explicit `ranvier-http` edge surface and deferred richer hybrid mount abstractions because the existing policy set already covers the validated use cases.

### Fixed
- **OpenAPI auth hinting (M386):** `AuthGuard` no longer implies bearer auth by name alone; OpenAPI security output now follows the guard integration's explicit `security_scheme_hint`.
- **SSE response extraction (M387):** Successful SSE responses now run guard response extractors, preserving metadata such as request IDs on realtime endpoints.

### Documentation
- **Milestone closure evidence (M385-M388):** Added completion evidence and updated milestone references for guard visibility, OpenAPI parity, realtime operability, and hybrid/static deferment.

---

## [0.37.0] — 2026-03

### Summary

**Ranvier 0.37.0 — StreamingTransition Sprint.**
Introduces the StreamingTransition trait for producing item streams instead of single Outcome values. StreamingAxon runtime type, SSE HTTP endpoints (post_sse/post_sse_typed), stream timeouts (init/idle/total), ranvier-test streaming support, and CLI modernization.

### Added
- **StreamingTransition trait (ranvier-core, M306, feature: `streaming`):** New trait `StreamingTransition<From>` with `run_stream()` returning `Pin<Box<dyn Stream<Item>>>`. `StreamEvent<T>` protocol enum (Data/Error/Done), `StreamTimeoutConfig` (init/idle/total), `StreamError` struct. `NodeKind::StreamingTransition` variant in Schematic with `item_type` and `terminal` fields.
- **StreamingAxon runtime (ranvier-runtime, M307, feature: `streaming`):** `StreamingAxon<In, Item, E, Res>` type wrapping an Axon prefix + StreamingTransition terminal step. `Axon::then_stream()` and `then_stream_with_timeout()` terminal builder methods. `StreamingAxonError<E>` enum (PipelineFault, UnexpectedOutcome, StreamInitError, Timeout). `TimeoutStream` wrapper enforcing init/idle/total timeouts. `collect_into_vec()` collapses stream to `Axon<In, Vec<Item>, String, Res>`.
- **SSE HTTP endpoints (ranvier-http, M307, feature: `streaming`):** `post_sse(path, streaming_axon)` and `post_sse_typed::<T>(path, streaming_axon)` methods on HttpIngress. SSE frame format: `data: {json}\n\n` per item + `data: [DONE]\n\n` sentinel. Backpressure via bounded mpsc channel (default 64 items). Client disconnect detection.
- **streaming-demo example (M308):** LLM chat streaming demo with `ChatRequest -> ClassifyIntent -> SynthesizeStream` SSE pipeline + batch JSON comparison endpoint.
- **ranvier-test streaming (M308, feature: `streaming`):** `TestAxon::run_stream()` method, `assert_stream_items!` macro for count and content assertions.
- **Cookbook streaming patterns guide (docs, M308):** EN/KO guide covering StreamingTransition, StreamingAxon, SSE endpoints, timeouts, testing, feature flags.

### Changed
- **CLI modernization (M308):** Fullstack template updated to `Axon::typed` + `post_typed` + `bus_injector` pattern. Added `NodeKind::StreamingTransition` and `TimelineEvent::NodeTimeout` match arms.
- **CLI version:** 0.5.0 -> 0.6.0, ranvier-core dependency 0.30 -> 0.37.
- **Examples catalog:** 63 -> 64 entries (streaming-demo added).
- **Web documentation:** 104 pages indexed (was 102), 9889 words (was 9687).

---

## [0.36.0] — 2026-03

### Summary

**Ranvier 0.36.0 — API Discovery & Documentation Sprint.**
OpenAPI auto-schema extraction from `post_typed()` via schemars, path parameter auto-documentation, htmx Bus integration, pre-compressed static file serving, Range request support, 6 Cookbook practical guides (EN/KO), and DD-4 StreamingTransition architecture draft.

### Added
- **OpenAPI auto-schema extraction (ranvier-openapi, M296):** `post_typed::<T>()` / `put_typed()` / `patch_typed()` now capture `T: schemars::JsonSchema` request body schema automatically. Generates `requestBody` with `application/json` media type in OpenAPI spec.
- **OpenAPI path parameter auto-documentation (ranvier-openapi, M296):** Route paths with `:param` segments (e.g., `/api/orders/:id`) are automatically converted to OpenAPI `{param}` format with `PathParameter` objects (type: string, required: true).
- **OpenAPI response schema manual registration (ranvier-openapi, M296):** `DocumentedRoute` builder with `.response::<T>(status, description)` for annotating response types via schemars.
- **htmx Bus integration (ranvier-http, M297, feature: `htmx`):** `HxRequest`, `HxTarget`, `HxTrigger`, `HxCurrentUrl`, `HxBoosted` types auto-injected into Bus from htmx request headers. `HttpIngress::htmx_support()` registration method. Response headers (`HX-Redirect`, `HX-Refresh`, `HX-Retarget`) via `ResponseBusExtractor`.
- **Pre-compressed static file serving (ranvier-http, M297):** `serve_precompressed()` builder — prioritizes `.br` (Brotli) → `.gz` (Gzip) → original file with correct `Content-Encoding` header. Transparent to clients with `Accept-Encoding` negotiation.
- **Range request support (ranvier-http, M297):** `enable_range_requests()` builder — `Accept-Ranges: bytes` header, `Range: bytes=X-Y` parsing, 206 Partial Content responses with `Content-Range` header. Supports single-range requests for media streaming.
- **Cookbook guides (docs, M298):** 6 practical pattern guides (EN/KO = 12 files):
  - Guard Patterns — global/per-route, `guards![]`, custom Guard, auth patterns
  - HttpIngress Patterns — post vs post_typed, path params, bus_injector, serve_dir
  - Bus Patterns — access methods, newtype safety, DB pool sharing, testing
  - Saga Compensation — then_compensated, LIFO order, retry, PostgresPersistenceStore
  - LLM Pipeline — LlmTransition, parallel tools, PII filtering, conversation history
  - DB Migration — sqlx-cli/refinery setup, Ranvier tables, Docker Compose, CI/CD
- **DD-4 StreamingTransition Architecture draft (docs/discussion, M298):** Design decision document analyzing Option A (StreamingTransition trait — recommended), Option B (Outcome::Stream variant), Option C (HttpIngress SSE-only). Covers Schematic representation, Bus integration models, error handling strategies. Implementation target: v0.37.0.

### Changed
- **`openapi-demo` example:** Updated to demonstrate auto-schema extraction from `post_typed()` and path parameter documentation.
- **`docs-manifest.json`:** New "Cookbook" category with 6 entries between Guides and Integration.
- **Web documentation:** 100 pages indexed (was 88), 9311 words (was 5029).

---

## [0.35.0] — 2026-03

### Summary

**Ranvier 0.35.0 — Pipeline-First Middleware Sprint.**
New `ranvier-guard` crate (12th workspace crate) with 15 Guard Transition nodes that fully replace Tower middleware. `HttpIngress::guard()` API with automatic Bus↔HTTP wiring, per-route `guards![]` macro composition, and `GuardIntegration` trait for custom Guard registration.

### Added
- **`ranvier-guard` crate (new, M292):** Dedicated crate for Guard Transition nodes extracted from `ranvier-std`. Guards are `Transition<T, T>` nodes that intercept requests via Bus read/write, either passing through or faulting with typed rejections.
- **`GuardIntegration` trait + `HttpIngress::guard()` (ranvier-http, M292):** Infrastructure for registering Guards with automatic HTTP→Bus injection (BusInjector), Bus→HTTP response extraction (ResponseExtractor), and response body transformation (ResponseBodyTransformFn). OPTIONS preflight auto-handling when CorsGuard is registered.
- **5 existing Guards migrated (M292):** `CorsGuard`, `AccessLogGuard`, `SecurityHeadersGuard`, `IpFilterGuard`, `RateLimitGuard` — moved from `ranvier-std` to `ranvier-guard` with full GuardIntegration impls.
- **4 core new Guards (M293) — Tower complete replacement gate:**
  - `CompressionGuard`: Accept-Encoding negotiation (gzip/brotli/identity), writes `CompressionConfig` to Bus, ingress applies gzip compression via `ResponseBodyTransformFn`
  - `RequestSizeLimitGuard`: Content-Length validation (413 Payload Too Large), convenience constructors `max_2mb()` / `max_10mb()`
  - `RequestIdGuard`: X-Request-Id generation (UUID v4) or propagation, writes `RequestId` to Bus
  - `AuthGuard`: Bearer token / API key / custom validator authentication with `subtle::ConstantTimeEq` timing-safe comparison, `IamPolicy` enforcement (RequireRole/RequirePermission/Custom)
- **3 additional Guards (M294):**
  - `ContentTypeGuard`: Content-Type media type validation (415 Unsupported Media Type), `json()` / `form()` / `accept()` constructors
  - `TimeoutGuard`: Pipeline execution deadline, writes `TimeoutDeadline` to Bus, ingress enforces via `tokio::time::timeout()` (408 Request Timeout)
  - `IdempotencyGuard`: Duplicate request prevention with `IdempotencyCache` (Arc-shared TTL HashMap), cache hit skips circuit and returns cached response body
- **Per-route Guard API (ranvier-http, M294):** `get_with_guards()`, `post_with_guards()`, `put_with_guards()`, `delete_with_guards()`, `patch_with_guards()` methods with save/restore pattern for combining global + per-route Guards.
- **`guards![]` macro (ranvier-http, M294):** Convenience macro calling `GuardIntegration::register()` on each guard expression.
- **3 Tier 3 Guards (M295, feature-gated: `advanced`):**
  - `DecompressionGuard`: Gzip request body decompression via flate2
  - `ConditionalRequestGuard`: If-None-Match / If-Modified-Since → 304 Not Modified (RFC 7232)
  - `RedirectGuard`: 301/302 redirect rule matching with Location header
- **Example: `guard-integration-demo` (M292-M294):** Demonstrates all 12 default Guards (7 global + 3 per-route) with `guards![]` macro usage.

### Changed
- **Crate count:** 11 → 12 workspace crates (`ranvier-guard` added).
- **`ranvier-std`:** Guard nodes remain for backwards compatibility but `ranvier-guard` is the canonical location.
- **`ranvier` facade crate:** Re-exports `ranvier-guard` prelude.
- **Publish order:** T3 tier now includes `ranvier-guard` between `ranvier-std` and `ranvier-http`.

---

## [0.34.0] — 2026-03

### Summary

**Ranvier 0.34.0 — Developer Experience Sprint.**
Closure-based inline Transitions (`then_fn()`), typed HTTP body injection (`post_typed()`), Askama template rendering, static file serving enhancements (304/MIME/immutable cache), pipeline error context auto-injection, new `ranvier-test` utility crate.

### Added
- **`ClosureTransition<F>` + `Axon::then_fn()` (ranvier-runtime):** Lightweight closure wrapper implementing the `Transition` trait. Sync closures `Fn(In, &mut Bus) -> Outcome<Out, E>` can be chained with `then_fn("label", closure)` alongside traditional `#[transition]` macro steps. Eliminates boilerplate for simple data transformations.
- **`Axon::typed::<In, E>()` (ranvier-runtime):** Convenience constructor for pipelines with a typed input, creating `Axon<In, In, E>`. Pairs with `post_typed()` for end-to-end type-safe HTTP request handling.
- **`HttpIngress::post_typed::<T>()` / `put_typed()` / `patch_typed()` (ranvier-http):** Type-safe JSON body deserialization — request body auto-parsed as `T: DeserializeOwned` and passed as Axon input. Returns 400 Bad Request on parse failure.
- **`TemplateResponse<T>` (ranvier-http, feature-gated: `askama`):** Askama template wrapper implementing `IntoResponse`. Renders templates to `text/html` with 500 error fallback.
- **Static file serving enhancements (ranvier-http):**
  - `guess_mime()` expanded with 8 types: `.avif`, `.webp`, `.webm`, `.mp4`, `.map`, `.ts`, `.tsx`, `.yaml`
  - `directory_index("index.html")` — automatic index file serving on directory paths
  - 304 Not Modified — `If-None-Match` vs ETag comparison
  - `immutable_cache()` — hashed filename detection (`name.HASH.ext`) with 1-year immutable Cache-Control
- **`TransitionErrorContext` (ranvier-core):** Struct auto-injected into Bus on pipeline fault, capturing `pipeline_name`, `transition_name`, `step_index`. `tracing::error!` with structured fields on every fault.
- **`ranvier-test` crate (new):** Test utilities for Ranvier pipelines.
  - `TestBus::new().with(val)` — fluent Bus builder for test data injection
  - `TestAxon::run(axon, input, res, bus)` — single-call pipeline execution returning `(Outcome, Bus)`
  - `assert_outcome_ok!()` / `assert_outcome_err!()` — typed Outcome assertion macros
- **Example: `closure-transition-demo`** — Mixed closure + macro Transition pipeline with `post_typed()` usage.

### Changed
- **`reference-ecommerce-order` example:** `inventory_circuit()` converted from `#[transition]` macro to `then_fn()` closure, demonstrating ergonomic improvement.

---

## [0.33.0] — 2026-03

### Summary

**Ranvier 0.33.0 — Developer Confidence Sprint.**
Outcome variant dedicated example, Bus access pattern guide, `then_with_timeout()` resilience method, service call demo (HTTP client as Transition), production readiness checklist, v1.0 stabilization criteria draft, ranvier-runtime README overhaul.

### Added
- **`then_with_timeout()` (ranvier-runtime):** Axon-level execution time limit using `tokio::time::timeout`. Returns `Outcome::Fault` with user-provided error factory on timeout. `TimelineEvent::NodeTimeout` variant for observability. Complements existing `then_with_retry()`.
- **`TimelineEvent::NodeTimeout` (ranvier-core):** New timeline event variant capturing node_id, timeout_ms, and timestamp when a transition exceeds the configured duration.
- **Example: `outcome-variants-demo`** — Demonstrates all 5 Outcome variants (Next, Fault, Branch, Jump, Emit) with a ticket processing domain. Tier A canonical example.
- **Example: `resilience-patterns-demo`** — Demonstrates `then_with_retry()` with exponential backoff and `then_with_timeout()` with success/timeout scenarios. Combined pipeline with both resilience methods.
- **Example: `service-call-demo`** — HTTP client (reqwest) wrapped as a Transition with Outcome-based error mapping: 2xx → Next, 4xx → Branch, 5xx/network → Fault. Integration with `then_with_timeout()`.
- **Guide: `bus_access_patterns.md`** — Decision tree for Bus method selection (require/read/get/try_require), comparison table, anti-patterns.
- **Guide: `production_readiness_checklist.md`** — 7-category pre-deployment checklist (auth, security, observability, resilience, data, deployment, CI/CD) with 24+ items and GitHub Actions YAML template.
- **Discussion: `246_v1_stabilization_criteria.md`** — v1.0 stabilization criteria draft covering API compatibility, test coverage, production validation, community, and infrastructure targets.

### Changed
- **ranvier-runtime README.md** — Complete rewrite with Axon builder usage, resilience method reference, persistence store comparison table, and example links.
- **`state-persistence-demo`** — Added PostgresPersistenceStore production usage note in module docs.
- **`examples/README.md`** — Added `auth-transition`, `auth-tower-integration`, `resilience-patterns-demo`, `service-call-demo` to Tier B.

---

## [0.32.0] — 2026-03

### Summary

**Ranvier 0.32.0 — Security Hardening Sprint.**
SQL injection prevention, timing-attack-safe token comparison, XOR deprecation/feature-gate, RFC 6265 cookie parsing, `Sensitive<T>` release-mode redaction, Bus panic elimination, JWT secret enforcement in examples, studio-server JWT validation + CORS restriction, web security headers, privacy policy, SECURITY.md guide.

### Added
- **`SECURITY.md` — Comprehensive security guide** covering auth patterns, secret management, CORS, security headers, `Sensitive<T>` usage, Bus access policies, OWASP Top 10 mapping, and vulnerability reporting process.
- **CookieJar RFC 6265 compliance (ranvier-http):** Cookie name validation (`is_valid_cookie_name`), quoted-value handling (`unquote_cookie_value`), percent-decoding (`percent_decode_cookie`), `tracing::warn` for invalid cookie names. 6 new tests.
- **Studio-server JWT validation:** `jsonwebtoken` crate integration, `Claims` struct with `sub`/`role`, signature verification when `RANVIER_JWT_SECRET` is set, fallback to header-based role when not configured.
- **Studio-server CORS restriction:** `build_cors_layer()` replaces `CorsLayer::permissive()`, reads `RANVIER_CORS_ORIGINS` env var, defaults to localhost dev ports.
- **Web `_headers` file:** HSTS, X-Content-Type-Options, X-Frame-Options, Referrer-Policy, Permissions-Policy, Content-Security-Policy for Cloudflare Pages.
- **Web privacy policy pages:** EN/KO bilingual `/privacy` and KO-only `/ko/privacy` with GA data collection disclosure, cookie table, GDPR legal basis, data retention, user rights.
- **Examples README security notice:** Banner directing users to SECURITY.md before production deployment.

### Changed
- **`Sensitive<T>` serialization (ranvier-compliance):** Release builds serialize `"[REDACTED]"` instead of actual value; debug builds retain full serialization for development.
- **Bus policy violations no longer panic (ranvier-core):** `read()`, `read_mut()`, `has()`, `remove()` now use `tracing::error!` and return `None`/`false` instead of panicking. 2 new tests.
- **JWT secrets in examples:** 3 examples (`auth-jwt-role-demo`, `reference-todo-api`, `reference-ecommerce-order`) changed from hardcoded `const` to `LazyLock<String>` reading `JWT_SECRET` env var. `auth-tower-integration` removed `"default-secret-key"` fallback.
- **Web `svelte.config.js`:** `precompress: true`, `strict: true` for production hardening.
- **Web GA anonymize_ip:** Added `anonymize_ip: true` to gtag config.
- **Web footer:** Privacy policy links added to EN and KO pages.

### Security (Breaking Changes)
- **`XorEncryption` deprecated and feature-gated (ranvier-compliance):** Now behind `#[cfg(feature = "xor-demo")]` with `#[deprecated]` warning. Use AES-256-GCM (`aes-gcm` crate) for production encryption.
- **SQL injection prevention (ranvier-audit):** `PostgresAuditSink` validates table names with `[a-zA-Z_][a-zA-Z0-9_]{0,62}` regex at construction time.
- **Timing-attack-safe token comparison (ranvier-inspector):** `BearerAuth` uses `subtle::ConstantTimeEq` instead of `==` for bearer token validation.
- **Environment-aware error responses (ranvier-http):** `outcome_to_response()` uses `cfg!(debug_assertions)` — debug builds include `Debug` output, release builds return generic `"Internal server error"` JSON.

---

## [0.31.0] — 2026-03

### Summary

**Ranvier 0.31.0 — Framework philosophy formalized.**
"Opinionated Core, Flexible Edges" principle documentation, Transition vs Tower authentication examples, integration guides (Tower/actix/Axum), comprehensive auth comparison.

### Added
- **PHILOSOPHY.md** — "Opinionated Core, Flexible Edges" framework design principle (EN/KO)
  - Core Paradigm, Decision Framework, Decision Tree (6 sections)
  - Code examples: Transition-based vs Tower-based auth patterns
- **DESIGN_PRINCIPLES.md** — Architecture decision records (ADR format)
  - DP-1: Paradigm Test (23→10 crate consolidation rationale)
  - DP-2: Tower Separation (Hyper 1.0 native migration rationale)
  - DP-3: Opinionated Core (non-negotiable paradigm enforcement)
- **Examples: Authentication patterns (2 approaches)**
  - `examples/auth-transition/` — Transition/Outcome/Bus-based auth (Ranvier recommended)
    - JWT validation, role-based authorization, Bus context propagation
    - 4 demo scenarios, Schematic visualization, README EN/KO
  - `examples/auth-tower-integration/` — Tower Service layer integration (ecosystem compatibility)
    - Two implementations: AsyncAuthorizeRequest (recommended) + manual Layer/Service (educational)
    - Tower validates → stores in request.extensions → Ranvier handles business logic
    - README EN/KO with trade-offs analysis
- **Guides: `docs/guides/auth-comparison.md`** — Transition vs Tower comparison (EN/KO)
  - 7-feature comparison table (context propagation, Schematic visualization, ecosystem compatibility, testing, etc.)
  - Performance benchmark (both ~1-2μs overhead, negligible)
  - When to use which: Decision tree, migration paths
  - Real-world examples: E-commerce platform (Transition), SaaS migration (Tower)
- **Web: Integration Guides** (`ranvier.rs/guides/integration`)
  - Landing page: "Flexible Edges in Action" (EN/KO)
  - Tower integration guide: Service/Layer patterns, auth example walkthrough (EN/KO)
  - actix-web integration guide: Extractor + Transition patterns, code snippets (EN/KO)
  - Axum integration guide: State sharing, extractor usage, code snippets (EN/KO)

### Changed
- **README.md**: Philosophy section added (links to PHILOSOPHY.md)
- **Web navigation**: "Integration" menu item added (routes to `/guides/integration`)
- **Web code-blocks.ts**: Integration example code blocks (Tower, actix, Axum)
- **Example count**: 61 → 63 examples (added auth-transition, auth-tower-integration)

### Notes
- **No crate code changes**: Documentation and examples only (version bump for API contract formalization)
- **No crates.io publish**: Local workspace version 0.31.0 for Git tag, crate code unchanged from 0.30.0
- **Philosophy documentation as API contract**: PHILOSOPHY.md clarifies when to use Ranvier paradigm vs ecosystem tools

---

## [0.30.0] — 2026-03

### Summary

**Ranvier 0.30.0 — Framework DX completion.**
`Axon::simple()` convenience constructor, HttpIngress rustdoc grouping, `telemetry-otel-demo` example, web manual operations/deployment pages (EN/KO), remaining `unwrap()` → `expect()` conversions.

### Added
- **`Axon::simple::<E>(label)` convenience constructor (M244):** Reduces `Axon::<(), (), String>::new("name")` to `Axon::simple::<String>("name")` for the most common pipeline pattern (no input state, no resources). Method-level generic supports turbofish syntax.
- **HttpIngress rustdoc section headers (M244):** 36 public methods organized into 10 categories (Server Configuration, Policies & Intervention, Lifecycle Hooks, Middleware Layers, Introspection, Static Assets, WebSocket, Health & Readiness, Routing, Execution) via section-header comments.
- **`telemetry-otel-demo` example (M245):** Demonstrates `RanvierConfig` 4-layer loading, `init_logging()`, `TelemetryConfig` OTLP auto-initialization, and `Axon::simple()` convenience constructor.
- **Web manual operations page EN/KO (M246):** Configuration system, health & readiness, request pipeline, structured logging, telemetry & OTLP.
- **Web manual deployment page EN/KO (M246):** Docker multi-stage builds, Kubernetes manifests, environment configuration, health probes.

### Changed
- **Example count:** 60 → 61 examples (added telemetry-otel-demo).
- **`ranvier-http` README:** "Tower-native" → "Hyper 1.0 native" (accuracy fix).
- **Examples README:** Updated to v0.30.0, added `telemetry-otel-demo` to Tier B.

### Fixed
- **11 `debug.rs` `unwrap()` → `expect("debug mutex poisoned")` (M244):** All Mutex lock calls in `DebugControl` now use descriptive panic messages.

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
