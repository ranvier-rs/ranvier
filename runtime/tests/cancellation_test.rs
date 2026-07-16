use async_trait::async_trait;
use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::saga::SagaPolicy;
use ranvier_core::{Bus, BusAccessPolicy, Outcome, Transition};
use ranvier_runtime::persistence::{
    CompensationContext, CompensationHandle, CompensationHook, CompletionState,
    InMemoryPersistenceStore, PersistenceHandle, PersistenceStore, PersistenceTraceId,
};
use ranvier_runtime::{Axon, ExecutionTerminal, ParallelStrategy};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone)]
struct WaitForCancellation {
    entered: Arc<Notify>,
    observed_token: Arc<AtomicUsize>,
}

#[async_trait]
impl Transition<i32, i32> for WaitForCancellation {
    type Error = String;
    type Resources = ();

    fn bus_access_policy(&self) -> Option<BusAccessPolicy> {
        Some(BusAccessPolicy::allow_only(Vec::new()))
    }

    async fn run(&self, state: i32, _resources: &(), bus: &mut Bus) -> Outcome<i32, String> {
        if bus.cancellation_token().is_some() {
            self.observed_token.fetch_add(1, Ordering::SeqCst);
        }
        self.entered.notify_one();
        std::future::pending::<()>().await;
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct RecordingCompensation {
    calls: Arc<tokio::sync::Mutex<Vec<CompensationContext>>>,
}

#[async_trait]
impl CompensationHook for RecordingCompensation {
    async fn compensate(&self, context: CompensationContext) -> anyhow::Result<()> {
        self.calls.lock().await.push(context);
        Ok(())
    }
}

async fn cancel_after_entry(entered: Arc<Notify>, token: CancellationToken) {
    entered.notified().await;
    token.cancel(CancellationReason::OperatorShutdown);
}

#[tokio::test]
async fn pre_cancelled_persisted_execution_records_entry_before_terminal() {
    let observed = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryPersistenceStore::new());
    let store_dyn: Arc<dyn PersistenceStore> = store.clone();
    let token = CancellationToken::new();
    assert!(token.cancel(CancellationReason::Explicit));

    let axon =
        Axon::<i32, i32, String>::start("CancellationBeforeEntry").then(WaitForCancellation {
            entered: Arc::new(Notify::new()),
            observed_token: observed.clone(),
        });
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store_dyn));
    bus.insert(PersistenceTraceId::new("trace-pre-cancelled"));

    let terminal = axon.execute_cancellable(1, &(), &mut bus, token).await;
    assert!(matches!(terminal, ExecutionTerminal::Cancelled(_)));
    assert_eq!(observed.load(Ordering::SeqCst), 0);

    let persisted = store
        .load("trace-pre-cancelled")
        .await
        .expect("load trace")
        .expect("persisted trace");
    assert_eq!(
        persisted
            .events
            .iter()
            .map(|event| (event.step, event.outcome_kind.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "Enter"), (1, "Cancelled")]
    );
    assert_eq!(persisted.completion, Some(CompletionState::Cancelled));
}

#[tokio::test]
async fn in_flight_cancellation_is_structured_persisted_and_policy_visible() {
    let entered = Arc::new(Notify::new());
    let observed = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryPersistenceStore::new());
    let store_dyn: Arc<dyn PersistenceStore> = store.clone();
    let token = CancellationToken::new();
    let trigger = tokio::spawn(cancel_after_entry(entered.clone(), token.clone()));

    let axon = Axon::<i32, i32, String>::start("CancellationPersisted").then(WaitForCancellation {
        entered,
        observed_token: observed.clone(),
    });
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store_dyn));
    bus.insert(PersistenceTraceId::new("trace-cancelled"));

    let terminal = tokio::time::timeout(
        Duration::from_secs(2),
        axon.execute_cancellable(1, &(), &mut bus, token),
    )
    .await
    .expect("cancellable execution should terminate");
    trigger.await.expect("cancellation trigger");

    assert!(matches!(
        terminal,
        ExecutionTerminal::Cancelled(context)
            if context.reason == CancellationReason::OperatorShutdown
    ));
    assert_eq!(observed.load(Ordering::SeqCst), 1);

    let persisted = store
        .load("trace-cancelled")
        .await
        .expect("load trace")
        .expect("persisted trace");
    assert_eq!(
        persisted
            .events
            .iter()
            .map(|event| event.outcome_kind.as_str())
            .collect::<Vec<_>>(),
        vec!["Enter", "Cancelled"]
    );
    assert_eq!(persisted.completion, Some(CompletionState::Cancelled));
    assert_eq!(
        persisted.events[1]
            .payload
            .as_ref()
            .and_then(|payload| payload.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("operator_shutdown")
    );
}

#[tokio::test]
async fn cancellation_runs_external_compensation_and_marks_compensated() {
    let entered = Arc::new(Notify::new());
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let store = Arc::new(InMemoryPersistenceStore::new());
    let store_dyn: Arc<dyn PersistenceStore> = store.clone();
    let token = CancellationToken::new();
    let trigger = tokio::spawn(cancel_after_entry(entered.clone(), token.clone()));

    let axon =
        Axon::<i32, i32, String>::start("CancellationCompensated").then(WaitForCancellation {
            entered,
            observed_token: Arc::new(AtomicUsize::new(0)),
        });
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store_dyn));
    bus.insert(PersistenceTraceId::new("trace-cancel-compensated"));
    bus.insert(CompensationHandle::from_hook(RecordingCompensation {
        calls: calls.clone(),
    }));

    let terminal = axon.execute_cancellable(1, &(), &mut bus, token).await;
    trigger.await.expect("cancellation trigger");
    assert!(matches!(terminal, ExecutionTerminal::Cancelled(_)));

    let persisted = store
        .load("trace-cancel-compensated")
        .await
        .expect("load trace")
        .expect("persisted trace");
    assert_eq!(
        persisted
            .events
            .iter()
            .map(|event| event.outcome_kind.as_str())
            .collect::<Vec<_>>(),
        vec!["Enter", "Cancelled", "Compensated"]
    );
    assert_eq!(persisted.completion, Some(CompletionState::Compensated));
    let recorded = calls.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].fault_kind, "Cancelled");
}

#[tokio::test]
async fn isolated_parallel_branches_receive_framework_cancellation_control() {
    let entered = Arc::new(Notify::new());
    let observed = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::new();
    let branches: Vec<Arc<dyn Transition<i32, i32, Resources = (), Error = String> + Send + Sync>> =
        (0..2)
            .map(|_| {
                Arc::new(WaitForCancellation {
                    entered: entered.clone(),
                    observed_token: observed.clone(),
                }) as Arc<_>
            })
            .collect();
    let axon = Axon::<i32, i32, String>::start("CancellationParallel")
        .parallel(branches, ParallelStrategy::AllMustSucceed);

    let trigger_token = token.clone();
    let trigger_observed = observed.clone();
    let trigger = tokio::spawn(async move {
        while trigger_observed.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        trigger_token.cancel(CancellationReason::Explicit);
    });
    let mut bus = Bus::new();
    let terminal = axon.execute_cancellable(1, &(), &mut bus, token).await;
    trigger.await.expect("parallel cancellation trigger");

    assert!(matches!(
        terminal,
        ExecutionTerminal::Cancelled(context) if context.reason == CancellationReason::Explicit
    ));
    assert_eq!(observed.load(Ordering::SeqCst), 2);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SagaState(i32);

#[derive(Clone)]
struct SagaStep(&'static str);

#[async_trait]
impl Transition<SagaState, SagaState> for SagaStep {
    type Error = String;
    type Resources = ();

    fn label(&self) -> String {
        self.0.to_string()
    }

    async fn run(
        &self,
        mut state: SagaState,
        _resources: &(),
        _bus: &mut Bus,
    ) -> Outcome<SagaState, String> {
        state.0 += 1;
        Outcome::Next(state)
    }
}

#[derive(Clone)]
struct SagaUndo(&'static str, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>);

#[async_trait]
impl Transition<SagaState, ()> for SagaUndo {
    type Error = String;
    type Resources = ();

    async fn run(&self, _state: SagaState, _resources: &(), bus: &mut Bus) -> Outcome<(), String> {
        if bus
            .cancellation_token()
            .is_some_and(|token| !token.is_cancelled())
        {
            self.2.fetch_add(1, Ordering::SeqCst);
        }
        self.1
            .lock()
            .expect("saga result lock")
            .push(self.0.to_string());
        Outcome::Next(())
    }
}

#[derive(Clone)]
struct WaitSaga {
    entered: Arc<Notify>,
}

#[async_trait]
impl Transition<SagaState, SagaState> for WaitSaga {
    type Error = String;
    type Resources = ();

    async fn run(
        &self,
        state: SagaState,
        _resources: &(),
        _bus: &mut Bus,
    ) -> Outcome<SagaState, String> {
        self.entered.notify_one();
        std::future::pending::<()>().await;
        Outcome::Next(state)
    }
}

#[tokio::test]
async fn cancellation_rolls_back_completed_saga_steps_in_lifo_order() {
    let entered = Arc::new(Notify::new());
    let rollback_order = Arc::new(Mutex::new(Vec::new()));
    let cleanup_tokens = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::new();
    let trigger = tokio::spawn(cancel_after_entry(entered.clone(), token.clone()));
    let axon = Axon::<SagaState, SagaState, String>::new("CancellationSaga")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(
            SagaStep("step-1"),
            SagaUndo("undo-1", rollback_order.clone(), cleanup_tokens.clone()),
        )
        .then_compensated(
            SagaStep("step-2"),
            SagaUndo("undo-2", rollback_order.clone(), cleanup_tokens.clone()),
        )
        .then(WaitSaga { entered });

    let mut bus = Bus::new();
    let terminal = axon
        .execute_cancellable(SagaState(0), &(), &mut bus, token)
        .await;
    trigger.await.expect("saga cancellation trigger");
    assert!(matches!(terminal, ExecutionTerminal::Cancelled(_)));
    assert_eq!(
        rollback_order.lock().expect("saga result lock").as_slice(),
        ["undo-2", "undo-1"]
    );
    assert_eq!(cleanup_tokens.load(Ordering::SeqCst), 2);
    assert!(
        bus.cancellation_token()
            .is_some_and(|token| !token.is_cancelled()),
        "cleanup must be shielded from the workflow cancellation token"
    );
}
