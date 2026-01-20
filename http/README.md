# Ranvier HTTP (`ranvier-http`)

> **The Bridge:** Adapts the Core Engine to the HTTP World.

## 🎯 Purpose
`ranvier-http` connects the pure methods of `ranvier-core` to the real-world HTTP ecosystem (`hyper`, `tower`, `http`). It translates incoming network requests into a `ranvier_core::Context` and executes the pipeline.

## 🔑 Key Components
- **`RanvierService`:** A `tower::Service` implementation that wraps a `Pipeline`.
- **`/__ranvier/schema`:** A built-in introspection endpoint that serves the JSON Netlist to the Studio.
- **Request/Response Mapping:** Converts `hyper::Request` -> `Context` -> `hyper::Response`.

## 🚀 Development Direction
- **Serverless Support:** Future adapters for Cloudflare Workers (`ranvier-worker`) will live alongside or leverage this crate.
- **Middleware Integration:** Support for standard Tower middleware.
- **WebSockets:** Future support for real-time steps.
