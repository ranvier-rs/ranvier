use std::convert::Infallible;
use ranvier::prelude::*;
use ranvier_macros::transition;
use std::time::Duration;

#[transition]
async fn concurrent_task(_input: (), _res: &(), _bus: &mut Bus) -> Outcome<serde_json::Value, Infallible> {
    // Simulate a small I/O delay or async operation
    // This tests how well the Ranvier runtime handles many concurrent async tasks
    tokio::time::sleep(Duration::from_millis(5)).await;
    
    Outcome::Next(serde_json::json!({
        "status": "success",
        "processed_at": now_ms()
    }))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = "0.0.0.0:3003";
    println!("Starting Ranvier Benchmark Server (Scenario 4: High Concurrency) on {}", addr);

    let axon = Axon::<(), (), Infallible>::new("concurrency")
        .then(concurrent_task);

    Ranvier::http()
        .bind(addr)
        .route("/concurrency", axon)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
