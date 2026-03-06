# Ranvier Core (`ranvier-core`)

> **The Kernel:** Pure logic engine for the Ranvier Framework.

## 🎯 Purpose

`ranvier-core` is the foundation of the entire framework. It defines the abstract concepts required to build a "Circuit-First" decision engine. It is designed to be:

- **Protocol Agnostic:** Does not depend on Hyper, HTTP, or any specific transport.
- **Circuit-Based:** Everything is an `Axon` or a `Transition`.
- **Introspectable:** Capable of generating a self-describing "Schematic" (JSON Schema) of the business logic.

## 🔑 Key Components

- **`Axon` Trait:** The circuit interface. Defines how a component takes an `Input` and returns an `Outcome`.
- **`Transition` Trait:** The atomic unit of logic that drives state changes.
- **`Outcome` Enum:** The result of an execution (`Next`, `Branch`, `Fault`, `End`).
- **`Bus`:** A type-map container for passing state and context through a circuit.
- **`iam` module:** `AuthContext` and `AuthScheme` — authentication context absorbed from the removed `ranvier-auth` crate.
- **`tenant` module:** `TenantId`, `TenantExtractor`, `TenantResolver`, `IsolationPolicy` — multi-tenancy primitives absorbed from the removed `ranvier-multitenancy` crate.

## 🚀 Development Direction

- **Strict Separation:** Keep this crate free of transport-layer dependencies.
- **Type Safety:** Leverage Rust's type system to ensure valid state transitions.
- **Telemetry:** Built-in tracing for visual debugging in Ranvier Studio.

## Examples

- [`hello-world`](../examples/hello-world/) — HTTP ingress baseline
- [`typed-state-tree`](../examples/typed-state-tree/) — Typed state progression
- [`basic-schematic`](../examples/basic-schematic/) — Schematic export + runtime

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
