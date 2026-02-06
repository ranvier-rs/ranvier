use ranvier_core::prelude::*;
use ranvier_macros::transition;
use ranvier_runtime::Axon;
use std::time::Duration;

#[transition]
async fn step_one(input: i32) -> Outcome<i32, anyhow::Error> {
    Outcome::Next(input + 10)
}

#[transition]
async fn step_two(input: i32) -> Outcome<String, anyhow::Error> {
    Outcome::Next(format!("Result: {}", input))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use tracing_subscriber::prelude::*;

    let fmt_layer = tracing_subscriber::fmt::layer();
    let inspector_layer = ranvier_inspector::layer();

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(inspector_layer)
        .init();

    tracing::info!("Starting Studio Demo...");

    // Start with i32 -> i32 identity
    let info_axon = Axon::<i32, i32, anyhow::Error>::start("Studio Demo Circuit")
        .then(step_one)
        .then(step_two);

    // Now info_axon is Axon<i32, String, Error>

    let axon = info_axon.serve_inspector(9000);

    tracing::info!("Inspector running on http://localhost:9000/quick-view");
    tracing::info!("Raw endpoints: /schematic, /trace/public, /trace/internal");

    loop {
        tracing::info!("Executing Axon...");
        let _ = axon.execute(50, &(), &mut Bus::new()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
