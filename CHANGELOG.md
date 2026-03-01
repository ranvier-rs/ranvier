# Changelog

All notable changes to the `ranvier` facade crate will be documented in this file.

The format is based on Keep a Changelog, and this project follows SemVer pre-1.0 policy.

## [0.10.0-rc.1] - 2026-02-24

### Added
- Stabilized core Execution and Decision Engine APIs (Gate A).
- Typed fallback execution and error extraction in `ranvier-core`.
- `ranvier-job` background job scheduling functionality.
- `ranvier-session` cache and session management backends.
- Official extensions (`ranvier-auth`, `ranvier-guard`, `ranvier-openapi`) stabilized (Gate B).
- Graceful shutdown and lifecycle hooks.
- Ecosystem reference examples integration (Gate C).

### Changed
- Promoted `v0.9.x` APIs to `v0.10.0`.
- Transitioned static routing to decoupled `ranvier-http`.
- Cleaned up unstable APIs and added proper deprecation tags where necessary.

## [Unreleased]
