use http::Request;
use ranvier_core::{Bus, Circuit, Module};
use ranvier_macros::module;

// 1. Define atomic modules using the macro
#[module]
async fn log_start() {
    println!("[Module] Circuit started.");
}

#[module]
async fn process_data() {
    println!("[Module] Processing data...");
}

#[module]
async fn log_end() {
    println!("[Module] Circuit ended.");
}

#[tokio::main]
async fn main() {
    // 2. Build the circuit (Schematic)
    let circuit = Circuit::new("My First Schematic")
        .with_description("A simple proof-of-concept circuit")
        .wire(log_start)
        .wire(process_data)
        .wire(log_end);

    // 3. Inspect Metadata (The Netlist)
    let meta = circuit.metadata();
    println!("=== Schematic Definition (JSON) ===");
    println!("{}", serde_json::to_string_pretty(&meta.to_json()).unwrap());
    println!("===================================");

    // 4. Execute (Simulation)
    println!("\n=== Running Simulation ===");
    let req = Request::new(()); // Empty request
    let mut bus = Bus::new(req);

    let result = circuit.execute(&mut bus).await;
    println!("Result: {:?}", result);
}
