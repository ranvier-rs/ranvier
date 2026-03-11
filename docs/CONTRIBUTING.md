# Contributing to Ranvier

Thank you for considering contributing to Ranvier! This guide covers everything you need to get started.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Making Changes](#making-changes)
- [Code Style](#code-style)
- [Testing](#testing)
- [Pull Request Process](#pull-request-process)
- [Architecture Overview](#architecture-overview)
- [Extension Development](#extension-development)

---

## Code of Conduct

This project follows the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). Be respectful, constructive, and welcoming. Harassment of any kind is not tolerated.

---

## Getting Started

### Prerequisites

- **Rust 1.93.0+** (edition 2024)
- **Git** with submodule support
- **cargo-semver-checks** (optional, for API compatibility verification)

### Clone the Workspace

```bash
git clone --recurse-submodules https://github.com/ranvier-rs/ranvier-workspace.git
cd ranvier-workspace
```

The workspace contains multiple submodules:

| Directory | Purpose |
|---|---|
| `ranvier/` | Core Rust crates (the main codebase) |
| `docs/` | Documentation and roadmap |
| `web/` | Website (SvelteKit) |
| `cli/` | CLI tooling |
| `studio/` | Visual editor (Tauri) |
| `vscode/` | VS Code extension |

Most contributions target the `ranvier/` submodule.

---

## Development Setup

### Build All Crates

```bash
cd ranvier
cargo build --workspace
```

### Run Tests

```bash
cargo test --workspace
```

### Run Lints

```bash
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
```

### Workspace Lint Configuration

The workspace uses shared clippy configuration in the root `Cargo.toml`:

```toml
[workspace.lints.clippy]
type_complexity = "allow"
too_many_arguments = "allow"
collapsible_if = "allow"
result_large_err = "allow"
should_implement_trait = "allow"
large_enum_variant = "allow"
```

All crates inherit these via `[lints] workspace = true`. Do not override workspace lints in individual crates without discussion.

---

## Making Changes

### Branch Naming

Use descriptive branch names with a prefix:

| Prefix | Use |
|---|---|
| `feat/` | New features |
| `fix/` | Bug fixes |
| `refactor/` | Code improvements without behavior change |
| `docs/` | Documentation only |
| `test/` | Test additions or fixes |
| `ci/` | CI/CD changes |

Example: `feat/custom-session-store`, `fix/websocket-close-race`

### Commit Messages

Follow conventional commit style:

```
feat: add DynamoDB session store implementation

Implements SessionStore trait for AWS DynamoDB. Includes automatic
TTL-based expiration and configurable table names.
```

Prefixes: `feat`, `fix`, `refactor`, `docs`, `test`, `ci`, `chore`

---

## Code Style

### Rust Conventions

- **Edition 2024** — use modern Rust idioms (let chains, `?` operator, etc.)
- **`async_trait`** — use `#[async_trait]` for async trait methods
- **Error types** — use `thiserror` for library errors, `anyhow` only in examples/tests
- **Derive order** — `Debug, Clone, Serialize, Deserialize` (when applicable)
- **Imports** — group: std, external crates, internal crates, current crate

### Architecture Rules

1. **`ranvier-core` is protocol-agnostic** — never add `http`, `tonic`, or `async-graphql` dependencies to core
2. **Transitions are the plugin boundary** — new business logic goes into `Transition` implementations
3. **Tower for middleware** — HTTP middleware must implement `tower::Layer<S>` + `tower::Service`
4. **Bus for context propagation** — use `Bus` for passing request context to transitions, not global state
5. **Feature-gate heavy deps** — optional dependencies behind Cargo features

### Documentation

- All public items must have doc comments (`///`)
- Include at least one usage example in module-level docs
- Use `# Examples` and `# Errors` sections in doc comments where appropriate

---

## Testing

### Test Organization

| Location | Type |
|---|---|
| `src/**/*.rs` (inline `#[cfg(test)]`) | Unit tests |
| `tests/*.rs` | Integration tests |
| `examples/` | Runnable examples (tested in CI) |

### Writing Tests

- Use descriptive test names: `test_rate_limiter_blocks_after_threshold`
- For HTTP tests, use `TestApp` from `ranvier_http::prelude`:
  ```rust
  let app = TestApp::new(ingress, resources);
  let response = app.send(TestRequest::get("/path")).await.unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  ```
- Test transitions use `String` for error types (not `Infallible` — it doesn't satisfy Axon's `Serialize` bounds)
- All `Transition` output types must implement `Serialize + DeserializeOwned`

### Running Specific Tests

```bash
# Single crate
cargo test -p ranvier-http

# Single test
cargo test -p ranvier-http test_app_hello_world_flow

# With output
cargo test -p ranvier-core -- --nocapture
```

---

## Pull Request Process

### Before Submitting

1. Run the full quality gate locally:
   ```bash
   cargo fmt --all --check
   cargo clippy --workspace -- -D warnings
   cargo test --workspace
   ```

2. If you added a new crate, ensure it:
   - Is listed in workspace `members` in `ranvier/Cargo.toml`
   - Has `[lints] workspace = true`
   - Has `version.workspace = true` (or a valid independent version)

3. If you modified public API, run semver checks:
   ```bash
   cargo semver-checks --workspace
   ```

### PR Requirements

- **Title** — short, descriptive (under 70 chars)
- **Description** — explain the "why", not just the "what"
- **Tests** — add tests for new functionality, update tests for changed behavior
- **No breaking changes** without prior discussion in an issue

### Review Criteria

Reviewers will check:

1. **Correctness** — does the code do what it claims?
2. **API design** — is it consistent with existing patterns?
3. **Test coverage** — are edge cases covered?
4. **Protocol agnosticism** — does it maintain core boundaries?
5. **Performance** — are there unnecessary allocations or blocking calls?

### CI Checks

All PRs must pass:

- `ranvier-core-ci` — fmt, clippy, tests (Linux/macOS/Windows), examples
- `ranvier-semver-checks` — API compatibility (if published crates modified)

---

## Architecture Overview

```
ranvier-core          Protocol-agnostic foundation (Transition, Bus, Outcome)
    |
ranvier-runtime       Axon execution engine
    |
    ├── ranvier-http      HTTP/WS/SSE ingress (Tower-based)
    ├── ranvier-grpc      gRPC ingress (tonic)
    └── ranvier-graphql   GraphQL ingress (async-graphql)
    |
    └── extensions/
        ├── guard         CORS, rate-limit, security headers
        ├── auth          JWT, API key, RBAC
        ├── session       Cookie sessions
        ├── db            Database adapters (PgNode, TxPgNode)
        ├── observe       OTLP tracing, metrics, DLQ
        ├── openapi       OpenAPI 3.0 generation
        ├── synapse       TypeScript client generation
        ├── audit         Tamper-evident audit logs
        ├── inspector     Runtime introspection
        ├── redis         Redis utilities
        ├── multitenancy  Multi-tenant support
        ├── compliance    Regulatory compliance
        └── cluster       Distributed locking
```

For detailed architecture, see `docs/05_dev_plans/plugin_architecture_m177.md`.

---

## Extension Development

Want to build a Ranvier extension? The five extension points are:

1. **Custom Transition** — implement `Transition<From, To>` for business logic nodes
2. **Tower Layer** — implement `Layer<S>` + `Service<Request>` for HTTP middleware
3. **Bus Injector** — `Fn(&Parts, &mut Bus)` to bridge request context
4. **Adapter Node** — wrap external service traits into `Transition` (like `PgNode`)
5. **Code Generator** — read `Schematic` to produce client/doc artifacts

See `docs/05_dev_plans/plugin_architecture_m177.md` for full documentation with examples.

---

## Questions?

- Open an issue on [GitHub](https://github.com/ranvier-rs/ranvier/issues)
- Check existing [discussions](https://github.com/ranvier-rs/ranvier/discussions) for prior design decisions
- Review `docs/discussion/` for architectural context (200+ design documents)
