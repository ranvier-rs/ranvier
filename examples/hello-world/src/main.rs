/*!
# Hello World - Ranvier Flat API Example

## Purpose
Demonstrates the **Flat API** pattern (Discussion 192-193):
- User code depth ≤ 2
- `Ranvier::http()` is an Ingress Circuit Builder, not a web server

## Running
```bash
cargo run --bin hello-world
# Open http://127.0.0.1:3000/ in browser
```
*/

use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;

// ============================================================================
// 1. Define Simple Transitions (Business Logic)
// ============================================================================

/// First Transition: Generate greeting message
#[derive(Clone)]
struct Greet;

#[async_trait]
impl Transition<(), String> for Greet {
    type Error = anyhow::Error;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::Next("Hello, Ranvier!".to_string())
    }
}

/// Second Transition: Add emoji to message
#[derive(Clone)]
struct Exclaim;

#[async_trait]
impl Transition<String, String> for Exclaim {
    type Error = anyhow::Error;
    type Resources = ();

    async fn run(
        &self,
        state: String,
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<String, Self::Error> {
        Outcome::Next(format!("{} 🚀", state))
    }
}

// ============================================================================
// 2. Main - Build and Wire Circuits with Flat API
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing for observability
    tracing_subscriber::fmt::init();

    println!("=== Ranvier Flat API Demo ===\n");

    // 1. Logic Circuit (Flat, Declarative)
    //    This is the "what to do" - depth = 1
    //    Note: Axon::new("label") creates an identity Axon<T, T, E>
    //          We start with () and transform to String via transitions
    let hello = Axon::<(), (), anyhow::Error>::new("HelloWorld")
        .then(Greet)
        .then(Exclaim);

    println!("Circuit defined with {} nodes", hello.schematic.nodes.len());
    println!("Starting server on http://127.0.0.1:3000\n");

    // 2. Ingress Configuration (Also Flat)
    //    This is the "how it enters" - depth = 2
    Ranvier::http()
        .bind("127.0.0.1:3000")
        .route("/", hello)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
