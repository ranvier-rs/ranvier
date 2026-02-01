use ranvier_core::prelude::*;
use ranvier_std::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("Running Ranvier Standard Library Demo...");

    // Axon executes the chain: String -> FilterNode -> SwitchNode

    // 1. FILTER: Accept only string longer than 5 chars
    let filter = FilterNode::new(|s: &String| s.len() > 5);

    // 2. SWITCH: Branch based on content
    let switch = SwitchNode::new(|s: &String| {
        if s.contains("Hello") {
            "greeting".to_string()
        } else {
            "other".to_string()
        }
    });

    let axon = Axon::start("Hello Ranvier".to_string(), "Demo Axon")
        .then(LogNode::new("Start", "info"))
        .then(filter)
        .then(switch)
        .then(LogNode::new(
            "This should not be reached due to switch",
            "warn",
        ));

    let mut bus = Bus::new();
    let result = axon.execute(&mut bus).await;

    println!("Execution Result: {:?}", result);

    match result {
        Outcome::Branch(id, payload) => println!("Switched to: {} with payload: {:?}", id, payload),
        Outcome::Fault(e) => println!("Filtered out: {}", e),
        _ => println!("Unexpected outcome"),
    }

    Ok(())
}
