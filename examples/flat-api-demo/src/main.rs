//! # Flat API Macro Demo
//!
//! Demonstrates named transitions using `#[transition]` and explicit HTTP routing.
//!
//! ## Run
//! ```bash
//! cargo run -p flat-api-demo
//! ```
//!
//! ## Key Concepts
//! - `#[transition]` macro for concise transition definitions
//! - `Ranvier::http().get(...)` for explicit HTTP route registration
//! - Resource injection pattern with custom resource types

use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;

fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> anyhow::Result<Outcome<T, E>> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        anyhow::bail!("Execution requires reconciliation before retry");
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => anyhow::bail!("Execution requires reconciliation before retry"),
    }
}

// 1. Define Resources
#[derive(Clone, Default)]
struct MyResources {
    pub multiplier: i32,
}

// 2. Define Transitions with Macros
#[transition(resources = MyResources)]
async fn init_state(_input: ()) -> Outcome<i32, String> {
    Outcome::Next(10i32)
}

#[transition]
async fn multiply_by_res(input: i32, res: &MyResources) -> Outcome<i32, String> {
    Outcome::Next(input * res.multiplier)
}

#[transition(resources = MyResources)]
async fn add_three(input: i32) -> Outcome<i32, String> {
    Outcome::Next(input + 3)
}

#[transition(resources = MyResources)]
async fn attach_trace(input: i32, _bus: &mut Bus) -> Outcome<i32, String> {
    Outcome::Next(input)
}

#[transition(resources = MyResources)]
async fn to_string(input: i32) -> Outcome<String, String> {
    Outcome::Next(input.to_string())
}

// 3. Define the circuit; route metadata stays in the HTTP builder.
async fn math_circuit() -> Axon<(), String, String, MyResources> {
    Axon::<(), (), String, MyResources>::new("MathCircuit")
        .then(init_state)
        .then(multiply_by_res)
        .then(add_three)
        .then(attach_trace)
        .then(to_string)
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Flat API Final Macro Demo ===");

    // 4. Register the circuit with an explicit GET builder.
    let _ingress = Ranvier::http::<MyResources>().get("/math", math_circuit().await);
    println!("HTTP ingress built successfully.");

    // 5. Verify the math_circuit logic
    let axon = math_circuit().await;
    let mut bus = Bus::new();
    let res = MyResources { multiplier: 2 };

    let result = local_demo_outcome(axon.execute((), &res, &mut bus).await)?;

    println!("Input: (), Internal Start: 10, Multiplier: 2");
    println!("Result: {:?}", result); // (10 * 2) + 3 = 23 -> "23"

    assert!(matches!(result, Outcome::Next(ref s) if s == "23"));
    println!("Verification PASSED!");

    Ok(())
}
