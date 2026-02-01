use ranvier_core::prelude::*;
use ranvier_std::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("Running Ranvier Standard Library Demo...");

    // Axon executes the chain: String -> LogNode -> RandomBranchNode
    // if probability check passes (0.5) -> Next -> LogNode("Main Path")
    // if probability check fails -> Branch("alternative_path") -> Execution stops with Branch outcome
    let axon = Axon::start("Hello World".to_string(), "Demo Axon")
        .then(LogNode::new("Start", "info"))
        .then(RandomBranchNode::new(0.5, "alternative_path"))
        .then(LogNode::new("Main Path (Lucky!)", "info"));

    let mut bus = Bus::new();
    let result = axon.execute(&mut bus).await;

    println!("Execution Result: {:?}", result);

    match result {
        Outcome::Next(_) => println!("Finished on Main Path"),
        Outcome::Branch(id, payload) => println!("Branched to: {} with payload: {:?}", id, payload),
        _ => println!("Other outcome"),
    }

    Ok(())
}
