use axum::{Json, Router, routing::get};
use serde::Serialize;
use std::time::Duration;

#[derive(Serialize)]
struct ConcurrencyResponse {
    status: &'static str,
    processed_at: u64,
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/concurrency", get(concurrency_handler));

    let addr = "0.0.0.0:4003";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!(
        "Starting Axum Benchmark Server (Scenario 4: High Concurrency) on {}",
        addr
    );
    axum::serve(listener, app).await.unwrap();
}

async fn concurrency_handler() -> Json<ConcurrencyResponse> {
    // Simulate async I/O delay
    tokio::time::sleep(Duration::from_millis(5)).await;

    Json(ConcurrencyResponse {
        status: "success",
        processed_at: now_ms(),
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
