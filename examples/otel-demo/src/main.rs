use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_observe::init_stdout_tracing;
use tracing::instrument;

// Define a simple Transition
#[derive(Clone)]
struct AddOne;

#[async_trait]
impl Transition<i32, i32> for AddOne {
    type Error = std::convert::Infallible;

    // Optional: Add tracing to inner logic too
    #[instrument(skip(self, _bus))]
    async fn run(&self, state: i32, _bus: &mut Bus) -> Outcome<i32, Self::Error> {
        tracing::info!("Adding one to {}", state);
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        Outcome::Next(state + 1)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize Tracing
    init_stdout_tracing();
    tracing::info!("Tracing initialized. Starting Axon...");

    // 2. Define Axon
    let axon = Axon::<i32, i32, std::convert::Infallible>::start("CalculationCircuit")
        .then(AddOne)
        .then(AddOne);

    // 3. Execute
    let mut bus = Bus::new();
    let result = axon.execute(10, &mut bus).await;

    tracing::info!("Execution Result: {:?}", result);

    Ok(())
}
