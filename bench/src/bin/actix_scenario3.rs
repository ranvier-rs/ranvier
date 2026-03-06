use actix_web::{web, App, HttpServer, HttpResponse};
use serde::Serialize;

#[derive(Serialize, Clone)]
struct WorkflowState {
    final_counter: i32,
    history: Vec<String>,
    status: &'static str,
}

async fn workflow_handler() -> HttpResponse {
    // Step 1
    let mut counter = 1;
    let mut history = vec!["step1".to_string()];

    // Step 2
    counter *= 10;
    history.push("step2".to_string());

    // Step 3
    counter += 5;
    history.push("step3".to_string());

    HttpResponse::Ok().json(WorkflowState {
        final_counter: counter,
        history,
        status: "workflow-complete",
    })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("Starting Actix-web Benchmark Server (Scenario 3: Multi-step Workflow) on 0.0.0.0:5002");
    HttpServer::new(|| {
        App::new().route("/workflow", web::get().to(workflow_handler))
    })
    .bind("0.0.0.0:5002")?
    .run()
    .await
}
