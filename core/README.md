# Ranvier Core (`ranvier-core`)

> **The Kernel:** Pure logic engine for the Ranvier Framework.

## 🎯 Purpose
`ranvier-core` is the foundation of the entire framework. It defines the abstract concepts required to build an "EDA (Electronic Design Automation)" style backend. It is designed to be:
- **Runtime Agnostic:** Does not depend on Tokio, Hyper, or any specific HTTP server.
- **Fractal:** Everything is a `Step` or a `Pipeline`.
- **Introspectable:** Capable of generating a self-describing "Netlist" (JSON Schema) of the business logic.

## 🔑 Key Components
- **`Step` Trait:** The atomic unit of work.
- **`Pipeline` Struct:** A recursive list of steps that implements `Step` itself.
- **`Context`:** A type-map container for passing state (`Request`, `Response`, `Extensions`) between steps.
- **`StepMetadata`:** Structural information (ID, connections) used by the Studio to visualize the code.

## 🚀 Development Direction
- **Strict Separation:** Keep this crate free of heavy dependencies.
- **State Management:** Enhance `Context` to support advanced dependency injection.
- **Telemetry:** Built-in tracing for visual debugging in Ranvier Studio.
