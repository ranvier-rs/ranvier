use async_trait::async_trait;
use ranvier_core::cancellation::{CancellationReason, CancellationToken};
use ranvier_core::saga::SagaPolicy;
use ranvier_core::{Bus, BusAccessPolicy, Outcome, Transition};
use ranvier_runtime::persistence::{
    CompensationContext, CompensationHandle, CompensationHook, CompensationIdempotencyHandle,
    CompensationIdempotencyStore, CompletionState, InMemoryCompensationIdempotencyStore,
    InMemoryPersistenceStore, Intervention, PersistedTrace, PersistenceEnvelope, PersistenceHandle,
    PersistenceStore, PersistenceTraceId, ResumeCursor,
};
use ranvier_runtime::{
    Axon, CompletionConfirmation, EffectBoundary, ExecutionReport, ExecutionTerminal,
    ParallelStrategy, PersistenceOperation,
};
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

struct AppliedCompensationError(InMemoryCompensationIdempotencyStore);

#[async_trait]
impl CompensationIdempotencyStore for AppliedCompensationError {
    async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
        self.0.was_compensated(key).await
    }
    async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
        self.0.mark_compensated(key).await?;
        Err(anyhow::anyhow!("private-compensation-error"))
    }
}

#[tokio::test]
async fn pre_cancelled_cleanup_reports_applied_compensation_without_ack() {
    for with_trace_store in [false, true] {
        let observed = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let backing = InMemoryCompensationIdempotencyStore::new();
        let original: Arc<dyn CompensationIdempotencyStore> =
            Arc::new(AppliedCompensationError(backing.clone()));
        let trace_store = InMemoryPersistenceStore::new();
        let mut bus = Bus::new();
        if with_trace_store {
            bus.insert(PersistenceHandle::from_store(trace_store.clone()));
        }
        bus.insert(PersistenceTraceId::new("pre-cancelled-compensation"));
        bus.insert(CompensationIdempotencyHandle::from_arc(original.clone()));
        bus.insert(CompensationHandle::from_hook(RecordingCompensation {
            calls: calls.clone(),
        }));
        let token = CancellationToken::new();
        token.cancel(CancellationReason::Explicit);
        let axon =
            Axon::<i32, i32, String>::start("PreCancelledCompensation").then(WaitForCancellation {
                entered: Arc::new(Notify::new()),
                observed_token: observed.clone(),
            });
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            axon.execute_cancellable(1, &(), &mut bus, token),
        )
        .await
        .expect("cleanup must terminate");
        assert!(matches!(&report, ExecutionReport::Unconfirmed {
            result: ExecutionTerminal::Cancelled(context),
            operation: PersistenceOperation::CompensationWrite,
            effect: EffectBoundary::MayHaveRun,
         .. } if context.reason == CancellationReason::Explicit));
        assert_eq!(observed.load(Ordering::SeqCst), 0);
        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].fault_kind, "Cancelled");
        // Current key contract; separate identity migration remains open.
        let key = format!(
            "{}:{}:{}",
            recorded[0].trace_id, recorded[0].circuit, recorded[0].fault_kind
        );
        assert!(backing.was_compensated(&key).await.unwrap());
        let restored = bus.read::<CompensationIdempotencyHandle>().unwrap().store();
        assert!(Arc::ptr_eq(&restored, &original));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-compensation-error")
        );
        if with_trace_store {
            let trace = trace_store
                .load("pre-cancelled-compensation")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(trace.completion, Some(CompletionState::Compensated));
        }
    }
}

#[tokio::test]
async fn completed_trace_blocks_normal_and_cancellation_effects_for_all_terminal_states() {
    for completion in [
        CompletionState::Success,
        CompletionState::Fault,
        CompletionState::Cancelled,
        CompletionState::Compensated,
    ] {
        for mode in 0..3 {
            let store = InMemoryPersistenceStore::new();
            let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let axon =
                Axon::<i32, i32, String>::start("CompletedGuard").then(WaitForCancellation {
                    entered: Arc::new(Notify::new()),
                    observed_token: Arc::new(AtomicUsize::new(0)),
                });
            store
                .append(PersistenceEnvelope {
                    trace_id: "completed".into(),
                    circuit: "CompletedGuard".into(),
                    schematic_version: axon.schematic().schema_version.clone(),
                    step: 0,
                    node_id: None,
                    outcome_kind: "Enter".into(),
                    timestamp_ms: 0,
                    payload_hash: None,
                    payload: None,
                })
                .await
                .unwrap();
            store
                .save_intervention(
                    "completed",
                    Intervention {
                        target_node: axon.schematic().nodes[1].id.clone(),
                        payload_override: Some(serde_json::json!(100)),
                        timestamp_ms: 1,
                    },
                )
                .await
                .unwrap();
            store
                .complete("completed", completion.clone())
                .await
                .unwrap();
            let before = store.load("completed").await.unwrap().unwrap();
            let mut bus = Bus::new();
            bus.insert(PersistenceHandle::from_store(store.clone()));
            bus.insert(PersistenceTraceId::new("completed"));
            bus.insert(CompensationHandle::from_hook(RecordingCompensation {
                calls: calls.clone(),
            }));
            if mode == 0 {
                let report =
                    tokio::time::timeout(Duration::from_secs(2), axon.execute(0, &(), &mut bus))
                        .await
                        .expect("completed execution must reject before waiting transition");
                assert!(matches!(report, ExecutionReport::NotRequested {
                    result: Outcome::Emit(event, None),
                 .. } if event == "execution.resumption.completed_trace"));
            } else {
                let token = CancellationToken::new();
                if mode == 2 {
                    token.cancel(CancellationReason::Explicit);
                }
                let report = tokio::time::timeout(
                    Duration::from_secs(2),
                    axon.execute_cancellable(0, &(), &mut bus, token),
                )
                .await
                .expect("completed cleanup must not invoke waiting transition");
                if mode == 2 {
                    assert!(matches!(report, ExecutionReport::NotRequested {
                        result: ExecutionTerminal::Cancelled(context),
                     .. } if context.reason == CancellationReason::Explicit));
                } else {
                    assert!(matches!(report, ExecutionReport::NotRequested {
                        result: ExecutionTerminal::Outcome(Outcome::Emit(event, None)),
                     .. } if event == "execution.resumption.completed_trace"));
                }
            }
            assert!(
                calls.lock().await.is_empty(),
                "terminal trace cannot replay compensation"
            );
            assert_eq!(store.load("completed").await.unwrap().unwrap(), before);
        }
    }
}

#[derive(Clone, Default)]
struct FailedCursorPersistenceStore {
    loads: Arc<AtomicUsize>,
    appends: Arc<AtomicUsize>,
    completions: Arc<AtomicUsize>,
}

#[async_trait]
impl PersistenceStore for FailedCursorPersistenceStore {
    async fn append(&self, _envelope: PersistenceEnvelope) -> anyhow::Result<()> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn load(&self, _trace_id: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("database cursor unavailable"))
    }

    async fn resume(
        &self,
        _trace_id: &str,
        _resume_from_step: u64,
    ) -> anyhow::Result<ResumeCursor> {
        Err(anyhow::anyhow!("database cursor unavailable"))
    }

    async fn complete(&self, _trace_id: &str, _completion: CompletionState) -> anyhow::Result<()> {
        self.completions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn save_intervention(
        &self,
        _trace_id: &str,
        _intervention: Intervention,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("database cursor unavailable"))
    }
}

#[tokio::test]
async fn pre_cancelled_execution_does_not_write_when_persistence_cursor_load_fails() {
    let observed = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(FailedCursorPersistenceStore::default());
    let token = CancellationToken::new();
    assert!(token.cancel(CancellationReason::Explicit));

    let axon = Axon::<i32, i32, String>::start("CancellationCursorUnavailable").then(
        WaitForCancellation {
            entered: Arc::new(Notify::new()),
            observed_token: observed.clone(),
        },
    );
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(
        store.clone() as Arc<dyn PersistenceStore>
    ));
    bus.insert(PersistenceTraceId::new("trace-cursor-unavailable"));

    let terminal = axon.execute_cancellable(1, &(), &mut bus, token).await;

    assert!(matches!(
        terminal,
        ExecutionReport::Unconfirmed {
            result: ExecutionTerminal::Cancelled(_),
            operation: PersistenceOperation::Load,
            effect: EffectBoundary::NotInvoked,
            ..
        }
    ));
    assert_eq!(observed.load(Ordering::SeqCst), 0);
    assert_eq!(store.loads.load(Ordering::SeqCst), 1);
    assert_eq!(store.appends.load(Ordering::SeqCst), 0);
    assert_eq!(store.completions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_entry_cursor_blocks_normal_and_cancellable_execution() {
    for cancellable in [false, true] {
        let store = Arc::new(FailedCursorPersistenceStore::default());
        let original: Arc<dyn PersistenceStore> = store.clone();
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let axon = Axon::<i32, i32, String>::start("UnknownEntry").then(WaitForCancellation {
            entered: Arc::new(Notify::new()),
            observed_token: Arc::new(AtomicUsize::new(0)),
        });
        let mut bus = Bus::new();
        bus.insert(PersistenceHandle::from_arc(original.clone()));
        bus.insert(PersistenceTraceId::new("unknown-entry"));
        bus.insert(CompensationHandle::from_hook(RecordingCompensation {
            calls: calls.clone(),
        }));
        let encoded = if cancellable {
            let report = tokio::time::timeout(
                Duration::from_secs(2),
                axon.execute_cancellable(0, &(), &mut bus, CancellationToken::new()),
            )
            .await
            .expect("unknown cursor must block before waiting transition");
            assert!(matches!(
                report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::Load,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ));
            serde_json::to_value(report).unwrap()
        } else {
            let report =
                tokio::time::timeout(Duration::from_secs(2), axon.execute(0, &(), &mut bus))
                    .await
                    .expect("unknown cursor must block before waiting transition");
            assert!(matches!(
                report,
                ExecutionReport::BlockedBeforeEffect {
                    operation: PersistenceOperation::Load,
                    effect: EffectBoundary::NotInvoked,
                    ..
                }
            ));
            serde_json::to_value(report).unwrap()
        };
        assert!(
            encoded.get("result").is_none(),
            "no fabricated terminal outcome"
        );
        assert!(!encoded.to_string().contains("database cursor unavailable"));
        assert_eq!(store.loads.load(Ordering::SeqCst), 1);
        assert_eq!(store.appends.load(Ordering::SeqCst), 0);
        assert_eq!(store.completions.load(Ordering::SeqCst), 0);
        assert!(calls.lock().await.is_empty());
        assert!(Arc::ptr_eq(
            &bus.read::<PersistenceHandle>().unwrap().store(),
            &original
        ));
    }
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
    assert!(matches!(
        terminal,
        ExecutionReport::Recorded {
            result: ExecutionTerminal::Cancelled(_),
            completion: CompletionConfirmation::Confirmed,
            ..
        }
    ));
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
        ExecutionReport::Recorded { result: ExecutionTerminal::Cancelled(context), completion: CompletionConfirmation::Confirmed , .. }
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
    assert!(matches!(
        terminal,
        ExecutionReport::Recorded {
            result: ExecutionTerminal::Cancelled(_),
            completion: CompletionConfirmation::Confirmed,
            ..
        }
    ));

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
        ExecutionReport::Unpersisted { result: ExecutionTerminal::Cancelled(context) , .. } if context.reason == CancellationReason::Explicit
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
    let owned_cleanup = terminal.cleanup().unwrap();
    assert!(owned_cleanup.is_complete());
    assert_eq!(owned_cleanup.steps.len(), 2);
    assert!(matches!(
        terminal,
        ExecutionReport::Unpersisted {
            result: ExecutionTerminal::Cancelled(_),
            ..
        }
    ));
    assert_eq!(
        rollback_order.lock().expect("saga result lock").as_slice(),
        ["undo-2", "undo-1"]
    );
    let cleanup = bus
        .read::<ranvier_runtime::saga_report::SagaRollbackReport>()
        .unwrap();
    assert!(cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 2);
    assert_eq!(cleanup_tokens.load(Ordering::SeqCst), 2);
    // cleanup_tokens above checks shielding during both undo operations.
    // After returning, restore the caller's originally absent owner.
    assert!(bus.cancellation_token().is_none());
}

#[derive(Clone)]
struct PendingShieldUndo(Arc<AtomicUsize>);
#[async_trait]
impl Transition<SagaState, ()> for PendingShieldUndo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: SagaState, _: &(), bus: &mut Bus) -> Outcome<(), String> {
        assert!(
            bus.cancellation_token()
                .is_some_and(|token| !token.is_cancelled())
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropped_shielded_undo_restores_parent_and_keeps_in_progress_journal() {
    use futures_util::FutureExt;
    use ranvier_runtime::saga_report::{SagaRollbackJournal, SagaRollbackOutcome};
    let undos = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<SagaState, SagaState, String>::new("shielded-drop")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(SagaStep("step"), PendingShieldUndo(undos.clone()))
        .then(WaitSaga {
            entered: Arc::new(Notify::new()),
        });
    let mut bus = Bus::new();
    let parent = CancellationToken::new();
    bus.set_cancellation_token(parent.clone());
    let child = CancellationToken::new();
    {
        let mut execution =
            Box::pin(axon.execute_cancellable(SagaState(0), &(), &mut bus, child.clone()));
        assert!(execution.as_mut().now_or_never().is_none());
        child.cancel(CancellationReason::Explicit);
        assert!(execution.as_mut().now_or_never().is_none());
    }
    assert_eq!(undos.load(Ordering::SeqCst), 1);
    let journal = bus.read::<SagaRollbackJournal>().unwrap().snapshot();
    assert!(!journal.finished);
    assert_eq!(journal.steps[0].outcome, SagaRollbackOutcome::InProgress);
    assert!(!bus.cancellation_token().unwrap().is_cancelled());
    parent.cancel(CancellationReason::Explicit);
    assert!(bus.cancellation_token().unwrap().is_cancelled());
}
