use async_trait::async_trait;
use ranvier_core::prelude::*;
use ranvier_core::saga::SagaPolicy;
use ranvier_runtime::Axon;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SagaState {
    cnt: i32,
    log: Vec<String>,
}

#[derive(Clone)]
struct SuccStep {
    id: String,
}

#[async_trait]
impl Transition<SagaState, SagaState> for SuccStep {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        format!("Step_{}", self.id)
    }
    async fn run(
        &self,
        mut state: SagaState,
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<SagaState, String> {
        state.cnt += 1;
        state.log.push(format!("Exec_{}", self.id));
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct CompStep {
    id: String,
    results: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Transition<SagaState, ()> for CompStep {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        format!("Comp_{}", self.id)
    }
    async fn run(&self, state: SagaState, _res: &(), _bus: &mut Bus) -> Outcome<(), String> {
        let mut res = self.results.lock().unwrap();
        // We verify that the state captured BEFORE this step is what we get
        res.push(format!("Comp_{}:{}", self.id, state.cnt));
        Outcome::Next(())
    }
}

#[derive(Clone)]
struct FailStep;

#[async_trait]
impl Transition<SagaState, SagaState> for FailStep {
    type Error = String;
    type Resources = ();
    fn label(&self) -> String {
        "FailStep".to_string()
    }
    async fn run(
        &self,
        _state: SagaState,
        _res: &(),
        _bus: &mut Bus,
    ) -> Outcome<SagaState, String> {
        Outcome::fault("Planned SAGA Failure".to_string())
    }
}

#[tokio::test]
async fn test_saga_automated_rollback_with_state_mapping() {
    let results = Arc::new(Mutex::new(Vec::new()));

    let step1 = SuccStep {
        id: "1".to_string(),
    };
    let comp1 = CompStep {
        id: "1".to_string(),
        results: results.clone(),
    };

    let step2 = SuccStep {
        id: "2".to_string(),
    };
    let comp2 = CompStep {
        id: "2".to_string(),
        results: results.clone(),
    };

    let axon = Axon::<SagaState, SagaState, String, ()>::new("SagaTest")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(step1, comp1)
        .then_compensated(step2, comp2)
        .then(FailStep);

    let initial_state = SagaState {
        cnt: 0,
        log: vec![],
    };
    let mut bus = Bus::new();
    let outcome = axon.execute(initial_state, &(), &mut bus).await;

    // 1. Verify it failed
    assert!(matches!(outcome, Outcome::Fault(_)));

    // 2. Verify rollbacks happened in LIFO order
    let final_results = results.lock().unwrap();
    assert_eq!(final_results.len(), 2);

    // Reverse order: Step 2 then Step 1
    // Step 2 was executed when cnt was 1 (it became 2). Snapshot before Step 2 should have cnt=1.
    // Step 1 was executed when cnt was 0 (it became 1). Snapshot before Step 1 should have cnt=0.

    assert_eq!(final_results[0], "Comp_2:1");
    assert_eq!(final_results[1], "Comp_1:0");
}
