use anyhow::Result;
use ranvier_core::prelude::*;
use ranvier_macros::{ranvier_router, route, transition};
use ranvier_runtime::Axon;

// 1. Define Resources
#[derive(Clone, Default)]
struct MyResources {
    pub multiplier: i32,
}

impl ranvier_core::transition::ResourceRequirement for MyResources {}

// 2. Define Transitions with Macros
#[transition(res = MyResources)]
async fn init_state(_input: ()) -> Outcome<i32, anyhow::Error> {
    Outcome::Next(10i32)
}

#[transition(res = MyResources)]
async fn multiply_by_res(input: i32, res: &MyResources) -> Outcome<i32, anyhow::Error> {
    Outcome::Next(input * res.multiplier)
}

#[transition(res = MyResources)]
async fn add_three(input: i32) -> Outcome<i32, anyhow::Error> {
    Outcome::Next(input + 3)
}

#[transition(res = MyResources)]
async fn to_string(input: i32) -> Outcome<String, anyhow::Error> {
    Outcome::Next(input.to_string())
}

// 3. Define Circuits with Routes
#[route(GET, "/math")]
async fn math_circuit() -> Axon<(), String, anyhow::Error, MyResources> {
    Axon::<(), (), anyhow::Error, MyResources>::start("MathCircuit")
        .then(init_state)
        .then(multiply_by_res)
        .then(add_three)
        .then(to_string)
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Flat API Final Macro Demo ===");

    // 4. Build Router automatically
    let _ingress = ranvier_router!(math_circuit);
    println!("Router built successfully.");

    // 5. Verify the math_circuit logic
    let axon = math_circuit().await;
    let mut bus = Bus::new();
    let res = MyResources { multiplier: 2 };

    let result = axon.execute((), &res, &mut bus).await;

    println!("Input: (), Internal Start: 10, Multiplier: 2");
    println!("Result: {:?}", result); // (10 * 2) + 3 = 23 -> "23"

    assert!(matches!(result, Outcome::Next(ref s) if s == "23"));
    println!("Verification PASSED!");

    Ok(())
}
