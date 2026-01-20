use ranvier_core::{Context, Pipeline, Step};
use ranvier_macros::step;
use http::Request;

// 1. Define atomic steps using the macro
#[step]
async fn log_start() {
    println!("[Step] Pipeline started.");
}

#[step]
async fn process_data() {
    println!("[Step] Processing data...");
}

#[step]
async fn log_end() {
    println!("[Step] Pipeline ended.");
}

#[tokio::main]
async fn main() {
    // 2. Build the circuit (Schematic)
    let pipeline = Pipeline::new("My First Schematic")
        .with_description("A simple proof-of-concept pipeline")
        .add_step(log_start)
        .add_step(process_data)
        .add_step(log_end);

    // 3. Inspect Metadata (The Netlist)
    let meta = pipeline.metadata();
    println!("=== Schematic Definition (JSON) ===");
    println!("{}", serde_json::to_string_pretty(&meta.to_json()).unwrap());
    println!("===================================");

    // 4. Execute (Simulation)
    println!("\n=== Running Simulation ===");
    let req = Request::new(()); // Empty request
    let mut ctx = Context::new(req);
    
    let result = pipeline.execute(&mut ctx).await;
    println!("Result: {:?}", result);
}
