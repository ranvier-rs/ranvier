use String;
use std::path::PathBuf;

use ranvier_core::prelude::*;
use ranvier_http::prelude::*;
use ranvier_runtime::Axon;
use serde_json::json;

#[derive(Clone)]
struct AcceptOrder;

#[async_trait::async_trait]
impl Transition<(), serde_json::Value> for AcceptOrder {
    type Error = Infallible;
    type Resources = ();

    async fn run(
        &self,
        _state: (),
        _resources: &Self::Resources,
        _bus: &mut Bus,
    ) -> Outcome<serde_json::Value, Self::Error> {
        Outcome::next(json!({
            "status": "accepted",
            "order_id": "ORDER-SUCCESS-999",
            "message": "Order received by embedded full-stack backend"
        }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("\n=== Ranvier Full-Stack Backend (Port 3030) ===\n");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let embedded_dir = manifest_dir.join("embedded");
    let assets_dir = embedded_dir.join("assets");
    let index_file = embedded_dir.join("index.html");

    if !index_file.exists() {
        anyhow::bail!("embedded index.html not found at {}", index_file.display());
    }
    if !assets_dir.exists() {
        anyhow::bail!(
            "embedded assets directory not found at {}",
            assets_dir.display()
        );
    }

    let order_route = Axon::<(), (), Infallible, ()>::new("AcceptOrder").then(AcceptOrder);

    println!("Serving embedded frontend at http://127.0.0.1:3030");
    println!("API endpoint: POST http://127.0.0.1:3030/api/order");

    Ranvier::http::<()>()
        .bind("127.0.0.1:3030")
        .serve_dir("/assets", assets_dir.to_string_lossy().to_string())
        .spa_fallback(index_file.to_string_lossy().to_string())
        .post("/api/order", order_route)
        .run(())
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    Ok(())
}
