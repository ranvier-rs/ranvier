use std::convert::Infallible;
use ranvier::prelude::*;
use ranvier_macros::transition;

#[derive(Clone, Default)]
struct WorkflowState {
    counter: i32,
    steps_completed: Vec<String>,
}

#[transition]
async fn step1(_input: (), _res: &(), _bus: &mut Bus) -> Outcome<WorkflowState, Infallible> {
    Outcome::Next(WorkflowState {
        counter: 1,
        steps_completed: vec!["step1".to_string()],
    })
}

#[transition]
async fn step2(mut state: WorkflowState, _res: &(), _bus: &mut Bus) -> Outcome<WorkflowState, Infallible> {
    state.counter *= 10;
    state.steps_completed.push("step2".to_string());
    Outcome::Next(state)
}

#[transition]
async fn step3(mut state: WorkflowState, _res: &(), _bus: &mut Bus) -> Outcome<serde_json::Value, Infallible> {
    state.counter += 5;
    state.steps_completed.push("step3".to_string());
    
    Outcome::Next(serde_json::json!({
        "final_counter": state.counter,
        "history": state.steps_completed,
        "status": "workflow-complete"
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = "0.0.0.0:3002";
    println!("Starting Ranvier Benchmark Server (Scenario 3: Multi-step Workflow) on {}", addr);

    let workflow_axon = Axon::<(), (), Infallible>::new("workflow")
        .then(step1)
        .then(step2)
        .then(step3);

    Ranvier::http()
        .bind(addr)
        .route("/workflow", workflow_axon)
        .run(())
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    Ok(())
}
