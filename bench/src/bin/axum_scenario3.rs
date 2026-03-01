use axum::{routing::get, Json, Router};
use serde::Serialize;

#[derive(Serialize, Clone)]
struct WorkflowState {
    final_counter: i32,
    history: Vec<String>,
    status: &'static str,
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/workflow", get(workflow_handler));

    let addr = "0.0.0.0:4002";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Starting Axum Benchmark Server (Scenario 3: Multi-step Workflow) on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

async fn workflow_handler() -> Json<WorkflowState> {
    // Step 1
    let mut counter = 1;
    let mut history = vec!["step1".to_string()];

    // Step 2
    counter *= 10;
    history.push("step2".to_string());

    // Step 3
    counter += 5;
    history.push("step3".to_string());

    Json(WorkflowState {
        final_counter: counter,
        history,
        status: "workflow-complete",
    })
}
