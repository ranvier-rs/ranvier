# Ranvier Macros (`ranvier-macros`)

> **The DX Layer:** Procedural macros for a seamless developer experience.

## 🎯 Purpose
Writing raw structs for every `Step` is verbose. `ranvier-macros` provides the "Syntax Sugar" that makes Rust code look and feel like a high-level circuit definition.

## 🔑 Key Components
- **`#[step]`:** transforms an `async fn` into a struct that implements `ranvier_core::Step`.
  - Automatically generates `StepMetadata`.
  - Handles dependency injection of `Context`.

## 🚀 Development Direction
- **Schema Inference:** Analyze function arguments to automatically populate `inputs` and `outputs` in the Netlist.
- **`#[pipeline]`:** A macro to define pipelines declaratively.
- **Validation:** Compile-time checks for circuit validity (e.g., type mismatches between steps).
