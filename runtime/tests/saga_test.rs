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

#[derive(Clone)]
struct CorruptLatestSagaSnapshot;

#[async_trait]
impl Transition<SagaState, SagaState> for CorruptLatestSagaSnapshot {
    type Error = String;
    type Resources = ();

    fn label(&self) -> String {
        "CorruptLatestSagaSnapshot".to_string()
    }

    async fn run(&self, state: SagaState, _res: &(), bus: &mut Bus) -> Outcome<SagaState, String> {
        if let Some(stack) = bus.read_mut::<ranvier_core::saga::SagaStack>()
            && let Some(task) = stack.tasks.last_mut()
        {
            task.input_snapshot = b"{ not valid json".to_vec();
        }
        Outcome::Next(state)
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
    let ranvier_runtime::ExecutionReport::Unpersisted { result: outcome } = outcome else {
        panic!("expected an execution without persistence");
    };

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

#[tokio::test]
async fn saga_compensation_snapshot_mismatch_does_not_panic() {
    let results = Arc::new(Mutex::new(Vec::new()));
    let axon = Axon::<SagaState, SagaState, String, ()>::new("SagaMismatch")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(
            SuccStep {
                id: "safe".to_string(),
            },
            CompStep {
                id: "safe".to_string(),
                results: results.clone(),
            },
        )
        .then(CorruptLatestSagaSnapshot)
        .then(FailStep);

    let initial_state = SagaState {
        cnt: 0,
        log: vec![],
    };
    let mut bus = Bus::new();
    let outcome = axon.execute(initial_state, &(), &mut bus).await;
    let ranvier_runtime::ExecutionReport::Unpersisted { result: outcome } = outcome else {
        panic!("expected an execution without persistence");
    };

    assert!(matches!(outcome, Outcome::Fault(_)));
    assert!(
        results.lock().unwrap().is_empty(),
        "malformed compensation snapshot should emit an event instead of running compensation"
    );
}

#[tokio::test]
async fn compensated_step_records_result_when_saga_is_disabled() {
    use ranvier_runtime::persistence::*;
    use ranvier_runtime::{CompletionConfirmation, ExecutionReport};
    let results = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(InMemoryPersistenceStore::default());
    let axon = Axon::<SagaState, SagaState, String, ()>::new("SagaDisabledPersistence")
        .with_saga_policy(SagaPolicy::Disabled)
        .then_compensated(
            SuccStep {
                id: "persist".into(),
            },
            CompStep {
                id: "persist".into(),
                results: results.clone(),
            },
        );
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    bus.insert(PersistenceTraceId::new("saga-disabled-result"));
    let report = axon
        .execute(
            SagaState {
                cnt: 0,
                log: vec![],
            },
            &(),
            &mut bus,
        )
        .await;
    let ExecutionReport::Recorded {
        result: Outcome::Next(state),
        completion: CompletionConfirmation::Confirmed,
    } = report
    else {
        panic!("expected acknowledged transition and completion");
    };
    assert_eq!(state.cnt, 1);
    let trace = store.load("saga-disabled-result").await.unwrap().unwrap();
    let events: Vec<_> = trace
        .events
        .iter()
        .filter(|event| event.node_id.is_some() && event.outcome_kind == "Next")
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome_kind, "Next");
    assert_eq!(
        events[0].payload,
        Some(Outcome::<SagaState, String>::Next(state).to_json_value())
    );
    assert!(results.lock().unwrap().is_empty());
}

#[tokio::test]
async fn compensated_step_records_fault_before_automatic_compensation() {
    use ranvier_runtime::persistence::*;
    use ranvier_runtime::{CompletionConfirmation, ExecutionReport};
    let results = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(InMemoryPersistenceStore::default());
    let axon = Axon::<SagaState, SagaState, String, ()>::new("SagaFaultPersistence")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(
            FailStep,
            CompStep {
                id: "fault".into(),
                results: results.clone(),
            },
        );
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    bus.insert(PersistenceTraceId::new("saga-fault-result"));
    let report = axon
        .execute(
            SagaState {
                cnt: 0,
                log: vec![],
            },
            &(),
            &mut bus,
        )
        .await;
    let ExecutionReport::Recorded {
        result: Outcome::Fault(error),
        completion: CompletionConfirmation::Confirmed,
    } = report
    else {
        panic!("expected acknowledged fault and completion");
    };
    assert_eq!(error, "Planned SAGA Failure");
    let trace = store.load("saga-fault-result").await.unwrap().unwrap();
    let fault_index = trace
        .events
        .iter()
        .position(|event| event.node_id.is_some() && event.outcome_kind == "Fault")
        .unwrap();
    let compensation_index = trace
        .events
        .iter()
        .position(|event| event.outcome_kind == "Compensated")
        .unwrap();
    assert!(fault_index < compensation_index);
    assert_eq!(
        trace.events[fault_index].payload,
        Some(Outcome::<SagaState, String>::Fault(error).to_json_value())
    );
    assert_eq!(*results.lock().unwrap(), vec!["Comp_fault:0"]);
}
