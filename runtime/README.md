# Ranvier Runtime (`ranvier-runtime`)

> **The Engine:** Async execution and state management for Ranvier circuits.

## 🎯 Purpose

`ranvier-runtime` provides the practical implementation for executing `Axon` circuits defined in `ranvier-core`. It handles:

- **Async Traversal**: Walking through the state tree asynchronously.
- **Bus Management**: Safely carrying the type-map state through transitions.
- **Tracing Integration**: Providing detailed execution logs and spans for debugging.

## 🔑 Key Components

- **`Axon` (Implementation)**: Concrete structures for building and nesting circuits.
- **`Bus` (Implementation)**: The thread-safe container for circuit state.
- **`Outcome` Handling**: Logic to resolve branches, loops, and faults during execution.

## 🚀 Usage

```rust
use ranvier_runtime::Axon;
use ranvier_core::prelude::*;

// Runtime is where the Axon builder and execution logic lives
let axon = Axon::start("MyCircuit")
    .then(some_transition);
```

## Examples

- [`hello-world`](../examples/hello-world/) — HTTP ingress baseline
- [`order-processing-demo`](../examples/order-processing-demo/) — Multi-step order processing pipeline

## MSRV

- Rust `1.93.0` or newer (Edition 2024).
