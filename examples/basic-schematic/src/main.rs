/*!
# Basic Schematic Example

## Purpose
Demonstrates Ranvier's core philosophy: **"Code structure = Execution structure"**.
When you define execution with Axon, a Schematic (structural analysis) is automatically generated.

## Learning Topics
- **Transition Trait**: Contract that defines state transformation
- **Axon Builder**: Compose execution paths with `start()` and `then()` chaining
- **Schematic Extraction**: JSON serialization of execution paths
- **Bus**: Context object for resource injection

## How to Run
```bash
cargo run --bin basic-schematic
```

## Expected Output
```
=== Schematic Definition (JSON) ===
{
  "name": "My First Schematic",
  "description": null,
  "nodes": [...],
  "edges": [...]
}
===================================
```

## Schematic vs Axon
| Concept | Role | Output |
|---------|------|--------|
| **Axon** | Execution path (dynamic) | `Outcome<T, E>` |
| **Schematic** | Structural analysis (static) | `Schematic` JSON |
*/

//! # Basic Schematic Demo - Axon/Schematic Example
//!
//! This example demonstrates the core Ranvier philosophy after the Axon/Schematic pivot:
//! > Code structure IS execution structure
//! > Execution flows through Axons; Structure is captured in Schematics
//!
//! This is a minimal example showing:
//! 1. Using the Axon builder to chain execution
//! 2. Implementing Transitions between states
//! 3. Using the Bus for resource injection
//! 4. Extracting Schematic metadata (the structure)

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_runtime::Axon;

// ============================================================================
// 1. Define Transitions (Atomic Steps)
// ============================================================================

/// Transition: () -> String (Log start)
#[derive(Clone)]
struct LogStart;

#[async_trait]
impl Transition<(), String> for LogStart {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        println!("[Axon] Circuit started.");
        Outcome::Next("Initial state".to_string())
    }
}

/// Transition: String -> String (Data processing)
#[derive(Clone)]
struct ProcessData;

#[async_trait]
impl Transition<String, String> for ProcessData {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        println!("[Axon] Processing data: {}", state);
        Outcome::Next(format!("Processed: {}", state))
    }
}

/// Transition: String -> () (Log end)
#[derive(Clone)]
struct LogEnd;

#[async_trait]
impl Transition<String, ()> for LogEnd {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<(), Self::Error> {
        println!("[Axon] Circuit ended with: {}", state);
        Outcome::Next(())
    }
}

// ============================================================================
// 2. Main - Build Axon and Execute
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Build the Axon (Execution chain)
    let axon = Axon::<(), (), String>::new("My First Schematic")
        .then(LogStart)
        .then(ProcessData)
        .then(LogEnd);

    if axon.maybe_export_and_exit()? {
        return Ok(());
    }

    // Extract Schematic (Static structure) before execution
    let schematic_json = serde_json::to_string_pretty(&axon.schematic)?;

    let node_count = axon.schematic.nodes.len();
    let edge_count = axon.schematic.edges.len();

    println!("=== Schematic Definition (JSON) ===");
    println!("{}", schematic_json);
    println!("===================================\n");

    // Execute the Axon
    println!("=== Running Axon ===");
    let mut bus = Bus::new();
    let result = axon.execute((), &(), &mut bus).await;
    println!("Final Result: {:?}", result);

    // Demonstrate Axon helper methods
    println!("\n=== Axon Analysis ===");
    println!("Total nodes: {}", node_count);
    println!("Total edges: {}", edge_count);

    Ok(())
}
