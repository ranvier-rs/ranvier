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
    let owned_cleanup = outcome.cleanup().cloned().unwrap();
    assert!(owned_cleanup.is_complete());
    assert_eq!(owned_cleanup.steps.len(), 2);
    let ranvier_runtime::ExecutionReport::Unpersisted {
        result: outcome, ..
    } = outcome
    else {
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
    drop(final_results);
    let cleanup = bus
        .read::<ranvier_runtime::saga_report::SagaRollbackReport>()
        .unwrap();
    assert!(cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 2);
    let next = Axon::<SagaState, SagaState, String>::new("ReusedBus");
    let _report = next
        .execute(
            SagaState {
                cnt: 0,
                log: vec![],
            },
            &(),
            &mut bus,
        )
        .await;
    assert!(
        bus.read::<ranvier_runtime::saga_report::SagaRollbackReport>()
            .is_none()
    );
    assert!(owned_cleanup.is_complete());
    assert_eq!(owned_cleanup.steps.len(), 2);
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
    let ranvier_runtime::ExecutionReport::Unpersisted {
        result: outcome, ..
    } = outcome
    else {
        panic!("expected an execution without persistence");
    };

    assert!(matches!(outcome, Outcome::Fault(_)));
    let report = bus
        .read::<ranvier_runtime::saga_report::SagaRollbackReport>()
        .unwrap();
    assert!(!report.is_complete());
    assert_eq!(report.steps.len(), 2);
    assert_eq!(
        report.steps[0].outcome,
        ranvier_runtime::saga_report::SagaRollbackOutcome::MissingHandler
    );
    assert_eq!(
        report.steps[1].outcome,
        ranvier_runtime::saga_report::SagaRollbackOutcome::Emitted
    );
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
        ..
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
        ..
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

#[derive(Clone)]
struct FaultUndo(Outcome<(), String>);
#[async_trait]
impl Transition<SagaState, ()> for FaultUndo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: SagaState, _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.clone()
    }
}

#[tokio::test]
async fn failed_automatic_compensation_does_not_claim_success() {
    for result in [
        Outcome::Fault("Undo failed".to_string()),
        Outcome::Emit(
            "undo.deferred".into(),
            Some(serde_json::json!({"ticket": 7})),
        ),
        Outcome::Branch("undo.branch".into(), None),
        Outcome::Jump(uuid::Uuid::nil(), None),
    ] {
        verify_incomplete_automatic_compensation(result).await;
    }
}

async fn verify_incomplete_automatic_compensation(compensation_result: Outcome<(), String>) {
    use ranvier_runtime::persistence::*;
    use ranvier_runtime::{CompletionConfirmation, ExecutionReport};
    let store = Arc::new(InMemoryPersistenceStore::new());
    let axon = Axon::<SagaState, SagaState, String>::new("FailedAutomaticUndo")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(FailStep, FaultUndo(compensation_result.clone()));
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    bus.insert(PersistenceTraceId::new("failed-automatic-undo"));
    bus.insert(ranvier_core::timeline::Timeline::new());
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
    let cleanup = report.cleanup().unwrap();
    assert!(cleanup.finished);
    assert!(!cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 1);
    assert_eq!(
        cleanup.steps[0].outcome,
        match &compensation_result {
            Outcome::Fault(_) => ranvier_runtime::SagaRollbackOutcome::Fault,
            Outcome::Emit(_, _) => ranvier_runtime::SagaRollbackOutcome::Emitted,
            Outcome::Branch(_, _) => ranvier_runtime::SagaRollbackOutcome::Branched,
            Outcome::Jump(_, _) => ranvier_runtime::SagaRollbackOutcome::Jumped,
            Outcome::Next(_) => unreachable!(),
        }
    );
    assert!(matches!(report, ExecutionReport::Recorded {
        result: Outcome::Fault(ref error), completion: CompletionConfirmation::Confirmed,
     .. } if error == "Planned SAGA Failure"));
    let trace = store.load("failed-automatic-undo").await.unwrap().unwrap();
    assert_eq!(trace.completion, Some(CompletionState::Fault));
    assert!(
        !trace
            .events
            .iter()
            .any(|event| event.outcome_kind == "Compensated")
    );
    let incomplete: Vec<_> = trace
        .events
        .iter()
        .filter(|event| event.outcome_kind == "CompensationIncomplete")
        .collect();
    assert_eq!(incomplete.len(), 1);
    let compensation_node = incomplete[0].node_id.as_ref().unwrap();
    let timeline = bus.read::<ranvier_core::timeline::Timeline>().unwrap();
    let exits: Vec<_> = timeline
        .events
        .iter()
        .filter_map(|event| match event {
            ranvier_core::timeline::TimelineEvent::NodeExit {
                node_id,
                outcome_type,
                ..
            } if node_id == compensation_node => Some(outcome_type.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(exits.len(), 1);
    assert_ne!(exits[0], "Compensated");
    let expected_type = match &compensation_result {
        Outcome::Fault(_) => "Fault".to_string(),
        Outcome::Emit(name, _) => format!("Emit:{name}"),
        Outcome::Branch(name, _) => format!("Branch:{name}"),
        Outcome::Jump(id, _) => format!("Jump:{id}"),
        Outcome::Next(_) => unreachable!("fixture only supplies incomplete control outcomes"),
    };
    assert_eq!(exits[0], expected_type);

    assert_eq!(
        incomplete[0].payload,
        Some(compensation_result.to_json_value())
    );
}

#[tokio::test]
async fn saga_rollback_classifies_incomplete_hooks_without_replacing_business_fault() {
    use ranvier_runtime::saga_report::{SagaRollbackOutcome, SagaRollbackReport};
    for (outcome, expected) in [
        (
            Outcome::Fault("private-cleanup-error".to_string()),
            SagaRollbackOutcome::Fault,
        ),
        (
            Outcome::Emit(
                "private-event".into(),
                Some(serde_json::json!({"secret": "private-payload"})),
            ),
            SagaRollbackOutcome::Emitted,
        ),
        (
            Outcome::Branch("branch".into(), None),
            SagaRollbackOutcome::Branched,
        ),
        (
            Outcome::Jump(uuid::Uuid::nil(), None),
            SagaRollbackOutcome::Jumped,
        ),
    ] {
        let axon = Axon::<SagaState, SagaState, String>::new("IncompleteRollback")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(SuccStep { id: "one".into() }, FaultUndo(outcome))
            .then(FailStep);
        let mut bus = Bus::new();
        let result = axon
            .execute(
                SagaState {
                    cnt: 0,
                    log: vec![],
                },
                &(),
                &mut bus,
            )
            .await;
        let owned_cleanup = result.cleanup().unwrap();
        assert!(!owned_cleanup.is_complete());
        assert_eq!(owned_cleanup.steps[0].outcome, expected);
        assert!(
            matches!(result, ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Fault(ref error) , .. } if error == "Planned SAGA Failure")
        );
        let report = bus.read::<SagaRollbackReport>().unwrap();
        assert!(!report.is_complete());
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].outcome, expected);
        let json = serde_json::to_string(report).unwrap();
        assert!(!json.contains("private-"));
        assert_eq!(
            serde_json::from_str::<SagaRollbackReport>(&json).unwrap(),
            *report
        );
    }
}

#[derive(Clone)]
struct PanicUndo;
#[async_trait]
impl Transition<SagaState, ()> for PanicUndo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: SagaState, _: &(), _: &mut Bus) -> Outcome<(), String> {
        panic!("private-panic-sentinel")
    }
}

#[tokio::test]
async fn rollback_panic_preserves_business_fault_and_incomplete_cleanup() {
    use ranvier_runtime::saga_report::{SagaRollbackOutcome, SagaRollbackReport};
    let completed_undo = Arc::new(Mutex::new(Vec::new()));
    let axon = Axon::<SagaState, SagaState, String>::new("PanicRollback")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(
            SuccStep {
                id: "earlier".into(),
            },
            CompStep {
                id: "earlier".into(),
                results: completed_undo.clone(),
            },
        )
        .then_compensated(SuccStep { id: "one".into() }, PanicUndo)
        .then(FailStep);
    let mut bus = Bus::new();
    let result = axon
        .execute(
            SagaState {
                cnt: 0,
                log: vec![],
            },
            &(),
            &mut bus,
        )
        .await;
    assert!(
        matches!(result, ranvier_runtime::ExecutionReport::Unpersisted { result: Outcome::Fault(ref error) , .. } if error == "Planned SAGA Failure")
    );
    let report = bus.read::<SagaRollbackReport>().unwrap();
    assert!(report.finished);
    assert!(!report.is_complete());
    assert_eq!(report.steps.len(), 2);
    assert_eq!(report.steps[0].outcome, SagaRollbackOutcome::Panicked);
    assert_eq!(report.steps[1].outcome, SagaRollbackOutcome::Completed);
    assert_eq!(*completed_undo.lock().unwrap(), vec!["Comp_earlier:0"]);
    assert!(
        !serde_json::to_string(report)
            .unwrap()
            .contains("private-panic")
    );
}

#[derive(Clone)]
struct PendingUndo(Arc<tokio::sync::Notify>);
#[async_trait]
impl Transition<SagaState, ()> for PendingUndo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: SagaState, _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropped_pending_rollback_retains_in_progress_without_completed_claim() {
    use ranvier_runtime::saga_report::SagaRollbackOutcome;
    let started = Arc::new(tokio::sync::Notify::new());
    let axon = Axon::<SagaState, SagaState, String>::new("PendingRollback")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(SuccStep { id: "one".into() }, PendingUndo(started.clone()))
        .then(FailStep);
    let mut bus = Bus::new();
    {
        let mut execution = Box::pin(axon.execute(
            SagaState {
                cnt: 0,
                log: vec![],
            },
            &(),
            &mut bus,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                _ = started.notified() => {},
                _ = &mut execution => panic!("pending cleanup unexpectedly returned"),
            }
        })
        .await
        .unwrap();
        // Dropping the polled executor releases its Bus borrow. No ACK or
        // known cleanup completion follows this interruption.
    }
    let report = bus
        .read::<ranvier_runtime::saga_report::SagaRollbackJournal>()
        .unwrap()
        .snapshot();
    assert!(!report.finished);
    assert!(!report.is_complete());
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].outcome, SagaRollbackOutcome::InProgress);
}
