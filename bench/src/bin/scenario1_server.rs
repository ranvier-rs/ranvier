use ranvier::prelude::*;
use ranvier_core::Never;
use ranvier_macros::transition;

#[transition]
async fn json_logic(_input: (), _res: &(), _bus: &mut Bus) -> Outcome<serde_json::Value, Never> {
    Outcome::Next(serde_json::json!({
        "message": "Hello, World!",
        "status": 200
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = "0.0.0.0:3000";
    println!(
        "Starting Ranvier Benchmark Server (Scenario 1: Simple CRUD) on {}",
        addr
    );

    let axon = Axon::<(), (), Never>::new("scenario1").then(json_logic);

    Ranvier::http()
        .bind(addr)
        .route("/", axon)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
