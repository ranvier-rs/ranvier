use ranvier_core::prelude::*;
use ranvier_std::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("Running Ranvier Standard Library Demo...");

    // Axon executes the chain: String -> LogNode -> String -> DelayNode -> String -> LogNode -> String
    let axon = Axon::start("Hello World".to_string(), "Demo Axon")
        .then(LogNode::new("Start", "info"))
        .then(DelayNode::new(1000))
        .then(LogNode::new("End", "info"));

    let mut bus = Bus::new();
    let result = axon.execute(&mut bus).await;

    println!("Result: {:?}", result);

    Ok(())
}
