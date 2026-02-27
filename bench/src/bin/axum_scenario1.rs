use axum::{routing::get, Json, Router};
use serde::Serialize;

#[derive(Serialize)]
struct SimpleResponse {
    message: String,
    status: u16,
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/", get(json_handler));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:4000").await.unwrap();
    println!("Starting Axum Benchmark Server (Scenario 1) on 0.0.0.0:4000");
    axum::serve(listener, app).await.unwrap();
}

async fn json_handler() -> Json<SimpleResponse> {
    Json(SimpleResponse {
        message: "Hello, World!".to_string(),
        status: 200,
    })
}
