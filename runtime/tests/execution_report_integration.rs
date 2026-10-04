use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{
    Axon, CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn cancellable_generated_identity_does_not_escape_invocation() {
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String>::new("identity-lifetime").then(Count(effects.clone()));
    let mut bus = Bus::new();
    for cancelled in [false, true, false] {
        let token = ranvier_core::cancellation::CancellationToken::new();
        if cancelled {
            token.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
        }
        let report = axon.execute_cancellable(1, &(), &mut bus, token).await;
        match report {
            ExecutionReport::Unpersisted { result, .. } => match result {
                ranvier_runtime::ExecutionTerminal::Cancelled(_) => assert!(cancelled),
                ranvier_runtime::ExecutionTerminal::Outcome(Outcome::Next(value)) => {
                    assert!(!cancelled);
                    assert_eq!(value, 2);
                }
                other => panic!("unexpected terminal: {other:?}"),
            },
            other => panic!("unexpected report: {other:?}"),
        }
        assert!(bus.read::<PersistenceTraceId>().is_none());
        assert!(bus.cancellation_token().is_none());
    }
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    bus.insert(PersistenceTraceId::new("caller-owned"));
    let _report = axon
        .execute_cancellable(
            1,
            &(),
            &mut bus,
            ranvier_core::cancellation::CancellationToken::new(),
        )
        .await;
    assert_eq!(bus.read::<PersistenceTraceId>().unwrap().0, "caller-owned");
    assert_eq!(effects.load(Ordering::SeqCst), 3);
    let parent = ranvier_core::cancellation::CancellationToken::new();
    bus.set_cancellation_token(parent.clone());
    for cancelled in [false, true] {
        let child = ranvier_core::cancellation::CancellationToken::new();
        if cancelled {
            child.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
        }
        let _ = axon.execute_cancellable(1, &(), &mut bus, child).await;
        assert!(!bus.cancellation_token().unwrap().is_cancelled());
    }
    parent.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
    assert!(bus.cancellation_token().unwrap().is_cancelled());
}

struct IdentityStore {
    backing: InMemoryPersistenceStore,
    keys: std::sync::Mutex<Vec<String>>,
}

#[derive(Clone)]
struct PendingInvocation;
#[async_trait::async_trait]
impl Transition<u32, u32> for PendingInvocation {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn external_future_drop_restores_handle_and_token() {
    use futures_util::FutureExt;
    let backing = InMemoryPersistenceStore::new();
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_store(backing));
    let original = bus.read::<PersistenceHandle>().unwrap().store();
    let parent = ranvier_core::cancellation::CancellationToken::new();
    bus.set_cancellation_token(parent.clone());
    let child = ranvier_core::cancellation::CancellationToken::new();
    let axon = Axon::<u32, u32, String>::new("external-drop").then(PendingInvocation);
    // Poll once through the first suspended transition, then drop the owned future.
    assert!(
        axon.execute_cancellable(1, &(), &mut bus, child.clone())
            .now_or_never()
            .is_none()
    );
    assert!(Arc::ptr_eq(
        &original,
        &bus.read::<PersistenceHandle>().unwrap().store()
    ));
    child.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
    assert!(!bus.cancellation_token().unwrap().is_cancelled());
    assert!(!parent.is_cancelled());
    assert!(bus.read::<ranvier_core::timeline::Timeline>().is_none());
    bus.insert(ranvier_core::timeline::Timeline::new());
    assert!(axon.execute(1, &(), &mut bus).now_or_never().is_none());
    assert!(
        !bus.read::<ranvier_core::timeline::Timeline>()
            .unwrap()
            .events
            .is_empty()
    );
    assert!(Arc::ptr_eq(
        &original,
        &bus.read::<PersistenceHandle>().unwrap().store()
    ));
    parent.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
    assert!(bus.cancellation_token().unwrap().is_cancelled());
}

#[async_trait::async_trait]
impl PersistenceStore for IdentityStore {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.keys.lock().unwrap().push(event.trace_id.clone());
        self.backing.append(event).await
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace, completion).await
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

#[tokio::test]
async fn repeated_cancellable_invocations_persist_separate_traces() {
    let store = Arc::new(IdentityStore {
        backing: InMemoryPersistenceStore::new(),
        keys: std::sync::Mutex::new(Vec::new()),
    });
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String>::new("persisted-identity").then(Count(effects.clone()));
    let mut bus = Bus::new();
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    let mut identities = Vec::new();
    let mut snapshots = Vec::new();
    for cancelled in [false, true, false] {
        store.keys.lock().unwrap().clear();
        let token = ranvier_core::cancellation::CancellationToken::new();
        if cancelled {
            token.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
        }
        let report = axon.execute_cancellable(1, &(), &mut bus, token).await;
        assert!(matches!(
            report,
            ExecutionReport::Recorded {
                completion: CompletionConfirmation::Confirmed,
                ..
            }
        ));
        let keys = store.keys.lock().unwrap().clone();
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|key| key == &keys[0]));
        assert!(!identities.contains(&keys[0]));
        identities.push(keys[0].clone());
        let trace = store.backing.load(&keys[0]).await.unwrap().unwrap();
        assert!(trace.completion.is_some());
        snapshots.push(serde_json::to_value(&trace).unwrap());
        for (identity, snapshot) in identities.iter().zip(&snapshots) {
            assert_eq!(
                serde_json::to_value(store.backing.load(identity).await.unwrap().unwrap()).unwrap(),
                *snapshot
            );
        }
        assert!(bus.read::<PersistenceTraceId>().is_none());
    }
    assert_eq!(effects.load(Ordering::SeqCst), 2);
}

#[derive(Clone, serde::Deserialize)]
struct EncodingOutput {
    terminal_only: bool,
    #[serde(skip)]
    calls: Arc<AtomicUsize>,
}
impl serde::Serialize for EncodingOutput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.terminal_only || call > 0 {
            Err(serde::ser::Error::custom("private-encoding-probe"))
        } else {
            serializer.serialize_u32(7)
        }
    }
}

#[derive(Clone)]
struct EncodingBusiness {
    effects: Arc<AtomicUsize>,
    terminal_only: bool,
}

#[derive(Clone)]
struct EncodingBranch(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<EncodingOutput, EncodingOutput> for EncodingBranch {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: EncodingOutput, _: &(), _: &mut Bus) -> Outcome<EncodingOutput, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(EncodingOutput {
            terminal_only: false,
            calls: Arc::new(AtomicUsize::new(0)),
        })
    }
}

#[tokio::test]
async fn parallel_encoding_failure_observes_branch_effects_and_skips_completion() {
    let backing = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("parallel-encoding"));
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    let axon = Axon::<EncodingOutput, EncodingOutput, String>::new("parallel-encoding").parallel(
        vec![
            Arc::new(EncodingBranch(effects.clone())),
            Arc::new(EncodingBranch(effects.clone())),
        ],
        ranvier_runtime::ParallelStrategy::AllMustSucceed,
    );
    let report = axon
        .execute(
            EncodingOutput {
                terminal_only: true,
                calls: Arc::new(AtomicUsize::new(0)),
            },
            &(),
            &mut bus,
        )
        .await;
    assert!(matches!(
        report,
        ExecutionReport::Unconfirmed {
            result: Outcome::Next(_),
            operation: PersistenceOperation::Result,
            effect: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    let trace = backing.load("parallel-encoding").await.unwrap().unwrap();
    assert_eq!(trace.completion, None);
    assert_eq!(trace.events.len(), 1);
    assert_eq!(trace.events[0].outcome_kind, "Enter");
}

#[derive(Clone)]
struct EncodingUndo(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl Transition<(), ()> for EncodingUndo {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(())
    }
}

#[derive(Clone)]
struct FaultAfterEncoding;
#[async_trait::async_trait]
impl Transition<EncodingOutput, ()> for FaultAfterEncoding {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: EncodingOutput, _: &(), _: &mut Bus) -> Outcome<(), String> {
        Outcome::Fault("original-business-fault".into())
    }
}

#[tokio::test]
async fn compensated_result_encoding_failure_keeps_undo_and_original_fault() {
    let backing = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let undos = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("compensated-encoding"));
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    let axon = Axon::<(), (), String>::new("compensated-encoding")
        .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
        .then_compensated(
            EncodingBusiness {
                effects: effects.clone(),
                terminal_only: false,
            },
            EncodingUndo(undos.clone()),
        )
        .then(FaultAfterEncoding);
    let report = axon.execute((), &(), &mut bus).await;
    assert!(report.cleanup().unwrap().is_complete());
    assert!(matches!(report, ExecutionReport::Unconfirmed {
        result: Outcome::Fault(ref fault), operation: PersistenceOperation::Result,
        effect: EffectBoundary::MayHaveRun, ..
    } if fault == "original-business-fault"));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(undos.load(Ordering::SeqCst), 1);
    let trace = backing.load("compensated-encoding").await.unwrap().unwrap();
    assert_eq!(trace.completion, None);
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.outcome_kind != "Next")
    );
    assert!(
        trace
            .events
            .iter()
            .any(|event| event.outcome_kind == "Fault" && event.node_id.is_none())
    );
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.payload != Some(serde_json::Value::Null))
    );
}

#[derive(Debug, Clone, serde::Deserialize)]
struct EncodingFault {
    private: bool,
}
impl serde::Serialize for EncodingFault {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.private {
            Err(serde::ser::Error::custom(
                "private-compensation-encoding-probe",
            ))
        } else {
            serializer.serialize_str("original-business-fault")
        }
    }
}
#[derive(Clone)]
struct FaultWithEncoding(Arc<AtomicUsize>, bool);
#[async_trait::async_trait]
impl Transition<(), ()> for FaultWithEncoding {
    type Error = EncodingFault;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<(), EncodingFault> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault(EncodingFault { private: self.1 })
    }
}

#[tokio::test]
async fn direct_compensation_encoding_failure_preserves_cleanup_and_domain_fault() {
    let backing = InMemoryPersistenceStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let undos = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("undo-encoding"));
    bus.insert(PersistenceHandle::from_store(backing.clone()));
    let axon = Axon::<(), (), EncodingFault>::new("undo-encoding").then_compensated(
        FaultWithEncoding(effects.clone(), false),
        FaultWithEncoding(undos.clone(), true),
    );
    let report = axon.execute((), &(), &mut bus).await;
    let cleanup = report.cleanup().unwrap();
    assert!(cleanup.finished);
    assert!(!cleanup.is_complete());
    assert_eq!(cleanup.steps.len(), 1);
    assert_eq!(
        cleanup.steps[0].outcome,
        ranvier_runtime::SagaRollbackOutcome::Fault
    );
    assert!(matches!(
        report,
        ExecutionReport::Unconfirmed {
            result: Outcome::Fault(EncodingFault { private: false }),
            operation: PersistenceOperation::Result,
            effect: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(undos.load(Ordering::SeqCst), 1);
    let trace = backing.load("undo-encoding").await.unwrap().unwrap();
    assert_eq!(trace.completion, None);
    assert_eq!(trace.events.len(), 3);
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.outcome_kind != "CompensationIncomplete")
    );
    let faults: Vec<_> = trace
        .events
        .iter()
        .filter(|event| event.outcome_kind == "Fault")
        .collect();
    assert_eq!(faults.len(), 2);
    assert!(faults.iter().any(|event| event.node_id.is_some()));
    assert!(faults.iter().any(|event| event.node_id.is_none()));
    assert!(faults.iter().all(
        |event| event.payload == Some(serde_json::json!({"Fault": "original-business-fault"}))
    ));
    assert!(
        trace
            .events
            .iter()
            .all(|event| event.payload != Some(serde_json::Value::Null))
    );
    assert!(
        !serde_json::to_string(&trace)
            .unwrap()
            .contains("private-compensation-encoding-probe")
    );
}
#[async_trait::async_trait]
impl Transition<(), EncodingOutput> for EncodingBusiness {
    type Error = String;
    type Resources = ();
    async fn run(&self, _: (), _: &(), _: &mut Bus) -> Outcome<EncodingOutput, String> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Outcome::Next(EncodingOutput {
            terminal_only: self.terminal_only,
            calls: Arc::new(AtomicUsize::new(0)),
        })
    }
}

#[tokio::test]
async fn result_and_terminal_encoding_failures_preserve_result_without_false_completion() {
    for terminal_only in [false, true] {
        let backing = InMemoryPersistenceStore::new();
        let effects = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new("encoding-failure"));
        bus.insert(PersistenceHandle::from_store(backing.clone()));
        let axon = Axon::<(), (), String>::new("encoding-fixture").then(EncodingBusiness {
            effects: effects.clone(),
            terminal_only,
        });
        let report = axon.execute((), &(), &mut bus).await;
        match report {
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(_),
                operation,
                effect,
                ..
            } => {
                assert_eq!(
                    operation,
                    if terminal_only {
                        PersistenceOperation::Terminal
                    } else {
                        PersistenceOperation::Result
                    }
                );
                assert_eq!(effect, EffectBoundary::MayHaveRun);
            }
            _ => panic!("unencodable recovery result must not be acknowledged"),
        }
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        let trace = backing.load("encoding-failure").await.unwrap().unwrap();
        assert_eq!(trace.completion, None);
        assert_eq!(trace.events.len(), if terminal_only { 2 } else { 1 });
        assert!(
            trace
                .events
                .iter()
                .all(|event| event.payload != Some(serde_json::Value::Null))
        );
        if terminal_only {
            assert_eq!(
                trace.events[1].payload,
                Some(serde_json::json!({"Next": 7}))
            );
        }
        // The restored store remains usable; failure state belongs to this
        // invocation, and cannot suppress completion of a later healthy call.
        bus.insert(PersistenceTraceId::new("after-encoding-failure"));
        let healthy = Axon::<(), (), String>::new("healthy-after-encoding");
        assert!(matches!(
            healthy.execute((), &(), &mut bus).await,
            ExecutionReport::Recorded {
                completion: CompletionConfirmation::Confirmed,
                ..
            }
        ));
        assert_eq!(
            backing
                .load("after-encoding-failure")
                .await
                .unwrap()
                .unwrap()
                .completion,
            Some(CompletionState::Success)
        );
    }
}

struct AppliedAwaitingAck {
    backing: InMemoryPersistenceStore,
    calls: AtomicUsize,
    block_at: usize,
    apply_blocked: bool,
    applied: Arc<tokio::sync::Notify>,
}

#[tokio::test]
async fn dropped_persistence_ack_restores_owner_without_rewriting_history() {
    use futures_util::FutureExt;
    for block_at in [0, 1, 2, 3, 4] {
        for applied in [false, true] {
            for cancellable in [false, true] {
                let backing = InMemoryPersistenceStore::new();
                let store = Arc::new(AppliedAwaitingAck {
                    backing: backing.clone(),
                    calls: AtomicUsize::new(0),
                    block_at,
                    apply_blocked: applied,
                    applied: Arc::new(tokio::sync::Notify::new()),
                });
                let effects = Arc::new(AtomicUsize::new(0));
                let axon = Axon::<u32, u32, String>::new("drop-ack").then(Count(effects.clone()));
                let mut bus = Bus::new();
                bus.insert(PersistenceHandle::from_arc(store.clone()));
                bus.insert(PersistenceTraceId::new("drop-ack"));
                let original = bus.read::<PersistenceHandle>().unwrap().store();
                if cancellable {
                    assert!(
                        axon.execute_cancellable(
                            1,
                            &(),
                            &mut bus,
                            ranvier_core::cancellation::CancellationToken::new()
                        )
                        .now_or_never()
                        .is_none()
                    );
                } else {
                    assert!(axon.execute(1, &(), &mut bus).now_or_never().is_none());
                }
                assert_eq!(store.calls.load(Ordering::SeqCst), block_at.min(3));
                assert_eq!(bus.read::<PersistenceTraceId>().unwrap().0, "drop-ack");
                assert_eq!(effects.load(Ordering::SeqCst), usize::from(block_at > 1));
                assert!(Arc::ptr_eq(
                    &original,
                    &bus.read::<PersistenceHandle>().unwrap().store()
                ));
                assert!(bus.cancellation_token().is_none());
                let trace = backing.load("drop-ack").await.unwrap();
                let expected = match block_at {
                    0 => 0,
                    4 => 3,
                    _ => block_at - 1 + usize::from(applied),
                };
                if expected == 0 {
                    assert!(trace.is_none());
                } else {
                    let trace = trace.unwrap();
                    assert_eq!(trace.events.len(), expected);
                    assert!(
                        trace
                            .events
                            .iter()
                            .all(|event| event.trace_id == "drop-ack")
                    );
                    assert_eq!(
                        trace.completion,
                        if block_at == 4 && applied {
                            Some(CompletionState::Success)
                        } else {
                            None
                        }
                    );
                }
            }
        }
    }
}
#[async_trait::async_trait]
impl PersistenceStore for AppliedAwaitingAck {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.block_at {
            if self.apply_blocked {
                self.backing.append(event).await?;
            }
            self.applied.notify_one();
            std::future::pending::<()>().await;
        } else {
            self.backing.append(event).await?;
        }
        Ok(())
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        if self.block_at == 0 {
            std::future::pending::<()>().await;
        }
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        if self.block_at == 4 {
            if self.apply_blocked {
                self.backing.complete(trace, completion.clone()).await?;
            }
            std::future::pending::<()>().await;
        }
        self.backing.complete(trace, completion).await
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

#[tokio::test]
async fn cancelled_applied_write_is_unconfirmed_without_invoking_transition() {
    verify_cancelled_write(
        1,
        PersistenceOperation::Intent,
        EffectBoundary::NotInvoked,
        0,
    )
    .await;
}

#[tokio::test]
async fn cancelled_result_write_preserves_already_invoked_effect_boundary() {
    verify_cancelled_write(
        2,
        PersistenceOperation::Result,
        EffectBoundary::MayHaveRun,
        1,
    )
    .await;
}

async fn verify_cancelled_write(
    block_at: usize,
    expected_operation: PersistenceOperation,
    expected_effect: EffectBoundary,
    effect_count: usize,
) {
    use ranvier_core::cancellation::{CancellationReason, CancellationToken};
    let backing = InMemoryPersistenceStore::new();
    let applied = Arc::new(tokio::sync::Notify::new());
    let effects = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("cancelled-ack"));
    bus.insert(PersistenceHandle::from_store(AppliedAwaitingAck {
        backing: backing.clone(),
        calls: AtomicUsize::new(0),
        block_at,
        apply_blocked: true,
        applied: applied.clone(),
    }));
    let token = CancellationToken::new();
    let axon = Axon::<u32, u32, String, ()>::new("cancelled-report").then(Count(effects.clone()));
    let execution = async {
        tokio::join!(
            axon.execute_cancellable(0, &(), &mut bus, token.clone()),
            async {
                applied.notified().await;
                assert!(token.cancel(CancellationReason::Explicit));
            }
        )
        .0
    };
    let report = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
        .await
        .unwrap();
    assert!(matches!(
        report,
        ExecutionReport::Unconfirmed {
            result: ranvier_runtime::ExecutionTerminal::Cancelled(_),
            operation,
            effect
        , .. } if operation == expected_operation && effect == expected_effect
    ));
    assert_eq!(effects.load(Ordering::SeqCst), effect_count);
    let persisted = backing.load("cancelled-ack").await.unwrap().unwrap();
    assert_eq!(persisted.events[0].outcome_kind, "Enter");
    assert_eq!(persisted.completion, Some(CompletionState::Cancelled));
    assert_eq!(persisted.events.len(), block_at + 1);
    assert_eq!(
        bus.read::<PersistenceHandle>()
            .unwrap()
            .store()
            .load("cancelled-ack")
            .await
            .unwrap(),
        Some(persisted)
    );
}

#[derive(Clone)]
struct Count(Arc<AtomicUsize>);

// Characterize the pre-existing fresh-input checkpoint policy. This is not
// acceptance of recovery from an incompatible persisted typed value.
#[tokio::test]
async fn checkpoint_fresh_input_can_mask_incompatible_persisted_value() {
    for compensated in [false, true] {
        for payload in [
            serde_json::json!({"Next": "invalid-u32"}),
            serde_json::json!({"Next": 99}),
        ] {
            let backing = InMemoryPersistenceStore::new();
            let effects = Arc::new(AtomicUsize::new(0));
            let previous_effects = Arc::new(AtomicUsize::new(0));
            let undo_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
            let prefix = Axon::<u32, u32, String>::new("typed-checkpoint")
                .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
                .then(Count(previous_effects.clone()));
            let axon = if compensated {
                prefix.then_compensated(Count(effects.clone()), RecordUndo(undo_inputs.clone()))
            } else {
                prefix.then(Count(effects.clone()))
            };
            backing
                .append(PersistenceEnvelope {
                    trace_id: "typed-checkpoint".into(),
                    circuit: axon.schematic().name.clone(),
                    schematic_version: axon.schematic().schema_version.clone(),
                    step: 1,
                    node_id: Some(axon.schematic().nodes[1].id.to_string()),
                    outcome_kind: "Next".into(),
                    timestamp_ms: 0,
                    payload_hash: None,
                    payload: Some(payload.clone()),
                })
                .await
                .unwrap();
            let mut bus = Bus::new();
            bus.insert(PersistenceTraceId::new("typed-checkpoint"));
            bus.insert(PersistenceHandle::from_store(backing.clone()));
            assert!(matches!(
                axon.execute(7, &(), &mut bus).await,
                ExecutionReport::Recorded {
                    result: Outcome::Next(8),
                    completion: CompletionConfirmation::Confirmed,
                    ..
                }
            ));
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            assert_eq!(previous_effects.load(Ordering::SeqCst), 0);
            assert!(undo_inputs.lock().unwrap().is_empty());
            let trace = backing.load("typed-checkpoint").await.unwrap().unwrap();
            assert_eq!(trace.completion, Some(CompletionState::Success));
            assert_eq!(trace.events[0].payload, Some(payload));
        }
    }
}

struct CountRecoveryCleanup(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl CompensationHook for CountRecoveryCleanup {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn legacy_missing_or_null_result_blocks_effects_without_rewriting_history() {
    for payload in [None, Some(serde_json::Value::Null)] {
        for mode in [0, 1, 2] {
            let backing = InMemoryPersistenceStore::new();
            let effects = Arc::new(AtomicUsize::new(0));
            let cleanup_calls = Arc::new(AtomicUsize::new(0));
            let axon = Axon::<u32, u32, String>::new("legacy-null").then(Count(effects.clone()));
            let schematic = axon.schematic();
            backing
                .append(PersistenceEnvelope {
                    trace_id: "legacy-null".into(),
                    circuit: schematic.name.clone(),
                    schematic_version: schematic.schema_version.clone(),
                    step: 1,
                    node_id: Some(schematic.nodes[1].id.to_string()),
                    outcome_kind: "Next".into(),
                    timestamp_ms: 0,
                    payload_hash: None,
                    payload: payload.clone(),
                })
                .await
                .unwrap();
            let before = serde_json::to_value(backing.load("legacy-null").await.unwrap()).unwrap();
            let mut bus = Bus::new();
            bus.insert(PersistenceTraceId::new("legacy-null"));
            bus.insert(PersistenceHandle::from_store(backing.clone()));
            bus.insert(CompensationHandle::from_hook(CountRecoveryCleanup(
                cleanup_calls.clone(),
            )));
            if mode != 0 {
                let token = ranvier_core::cancellation::CancellationToken::new();
                if mode == 2 {
                    let _ = token.cancel(ranvier_core::cancellation::CancellationReason::Explicit);
                }
                let report = axon.execute_cancellable(7, &(), &mut bus, token).await;
                assert!(matches!(
                    report,
                    ExecutionReport::BlockedBeforeEffect {
                        operation: PersistenceOperation::RecoveryPayload,
                        effect: EffectBoundary::NotInvoked,
                        ..
                    }
                ));
            } else {
                let report = axon.execute(7, &(), &mut bus).await;
                assert!(matches!(
                    report,
                    ExecutionReport::BlockedBeforeEffect {
                        operation: PersistenceOperation::RecoveryPayload,
                        effect: EffectBoundary::NotInvoked,
                        ..
                    }
                ));
            }
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            assert_eq!(cleanup_calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                serde_json::to_value(backing.load("legacy-null").await.unwrap()).unwrap(),
                before
            );
        }
    }
}

#[async_trait::async_trait]
impl Transition<u32, u32> for Count {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::next(input + 1)
    }
}

struct FailOnAppend {
    backing: InMemoryPersistenceStore,
    calls: Arc<AtomicUsize>,
    fail_on_append: usize,
    apply_failed_append: bool,
}

#[cfg(feature = "streaming")]
#[derive(Clone)]
struct CountStream(Arc<AtomicUsize>);
#[cfg(feature = "streaming")]
#[async_trait::async_trait]
impl ranvier_core::streaming::StreamingTransition<u32> for CountStream {
    type Item = u32;
    type Error = String;
    type Resources = ();
    async fn run_stream(
        &self,
        input: u32,
        _: &(),
        _: &mut Bus,
    ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::iter([input, input + 1])))
    }
}

#[cfg(feature = "streaming")]
#[tokio::test]
async fn collected_stream_prefix_terminal_failure_blocks_initialization_and_retains_known_result() {
    let backing = InMemoryPersistenceStore::new();
    let writes = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("stream-report"));
    bus.insert(PersistenceHandle::from_store(FailOnAppend {
        backing: backing.clone(),
        calls: writes.clone(),
        fail_on_append: 2,
        apply_failed_append: false,
    }));
    let axon = Axon::<u32, u32, String, ()>::new("stream-report")
        .then_stream(CountStream(effects.clone()))
        .collect_into_vec();
    let report = axon.execute(7, &(), &mut bus).await;
    assert!(matches!(report, ranvier_runtime::StreamInitializationReport::Rejected {
        prefix: ExecutionReport::Unconfirmed {
            result: ranvier_runtime::ExecutionTerminal::Outcome(Outcome::Next(7)),
            operation: PersistenceOperation::Terminal, effect: EffectBoundary::MayHaveRun, ..
        }
    }));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(writes.load(Ordering::SeqCst), 2);
    let trace = backing.load("stream-report").await.unwrap().unwrap();
    assert_eq!(trace.events.len(), 1);
    assert_eq!(trace.events[0].outcome_kind, "Enter");
    assert_eq!(trace.completion, None);
}

struct UnknownEntry {
    backing: InMemoryPersistenceStore,
    applied: bool,
    appends: Arc<AtomicUsize>,
    completions: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl PersistenceStore for UnknownEntry {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            event.outcome_kind, "Enter",
            "no subsequent write after unknown entry"
        );
        if self.applied {
            self.backing.append(event).await?;
        }
        Err(anyhow::anyhow!("private-entry-error"))
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.completions.fetch_add(1, Ordering::SeqCst);
        self.backing.complete(trace, completion).await
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

#[tokio::test]
async fn rejected_and_applied_unknown_entries_block_both_execution_paths() {
    for applied in [false, true] {
        for cancellable in [false, true] {
            let backing = InMemoryPersistenceStore::new();
            let writes = Arc::new(AtomicUsize::new(0));
            let completions = Arc::new(AtomicUsize::new(0));
            let effects = Arc::new(AtomicUsize::new(0));
            let original: Arc<dyn PersistenceStore> = Arc::new(UnknownEntry {
                backing: backing.clone(),
                applied,
                appends: writes.clone(),
                completions: completions.clone(),
            });
            let mut bus = Bus::new();
            bus.insert(PersistenceTraceId::new("unknown-entry"));
            bus.insert(PersistenceHandle::from_arc(original.clone()));
            let axon =
                Axon::<u32, u32, String, ()>::new("entry-guard").then(Count(effects.clone()));
            let encoded = if cancellable {
                let report = axon
                    .execute_cancellable(
                        0,
                        &(),
                        &mut bus,
                        ranvier_core::cancellation::CancellationToken::new(),
                    )
                    .await;
                assert!(matches!(
                    report,
                    ExecutionReport::BlockedBeforeEffect {
                        operation: PersistenceOperation::Intent,
                        effect: EffectBoundary::NotInvoked,
                        ..
                    }
                ));
                serde_json::to_value(report).unwrap()
            } else {
                let report = axon.execute(0, &(), &mut bus).await;
                assert!(matches!(
                    report,
                    ExecutionReport::BlockedBeforeEffect {
                        operation: PersistenceOperation::Intent,
                        effect: EffectBoundary::NotInvoked,
                        ..
                    }
                ));
                serde_json::to_value(report).unwrap()
            };
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            assert_eq!(writes.load(Ordering::SeqCst), 1);
            assert_eq!(completions.load(Ordering::SeqCst), 0);
            assert!(encoded.get("result").is_none());
            assert!(!encoded.to_string().contains("private-entry-error"));
            let actual = backing.load("unknown-entry").await.unwrap();
            if applied {
                let trace = actual.unwrap();
                assert_eq!(trace.events.len(), 1);
                assert_eq!(trace.events[0].outcome_kind, "Enter");
                assert_eq!(trace.completion, None);
            } else {
                assert!(actual.is_none());
            }
            assert!(Arc::ptr_eq(
                &bus.read::<PersistenceHandle>().unwrap().store(),
                &original
            ));
        }
    }
}
#[async_trait::async_trait]
impl PersistenceStore for FailOnAppend {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.fail_on_append {
            if self.apply_failed_append {
                self.backing.append(event).await?;
            }
            return Err(anyhow::anyhow!("secret-backend-sentinel"));
        }
        self.backing.append(event).await
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace).await
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.backing.complete(trace, completion).await
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

#[tokio::test]
async fn actual_execute_stops_after_unconfirmed_result_append() {
    for (trace_id, apply_failed_append) in [("not-applied", false), ("applied-lost-ack", true)] {
        let backing = InMemoryPersistenceStore::new();
        let writes = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_store(FailOnAppend {
            backing: backing.clone(),
            calls: writes.clone(),
            fail_on_append: 2,
            apply_failed_append,
        }));
        let axon = Axon::<u32, u32, String, ()>::new("report-fixture").then(Count(effects.clone()));
        let report = axon.execute(0, &(), &mut bus).await;
        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(1),
                operation: PersistenceOperation::Result,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ));
        assert!(!format!("{report:?}").contains("secret-backend-sentinel"));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(writes.load(Ordering::SeqCst), 2);

        let trace = backing.load(trace_id).await.unwrap().unwrap();
        assert_eq!(trace.completion, None);
        assert_eq!(trace.events.len(), if apply_failed_append { 2 } else { 1 });
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| event.outcome_kind == "Next")
                .count(),
            usize::from(apply_failed_append)
        );
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        if apply_failed_append {
            assert_eq!(trace.events[1].outcome_kind, "Next");
        }
        assert_eq!(
            bus.read::<PersistenceHandle>()
                .unwrap()
                .store()
                .load(trace_id)
                .await
                .unwrap(),
            Some(trace.clone())
        );
    }
}

#[tokio::test]
async fn actual_execute_reports_terminal_append_ack_loss_after_domain_result() {
    for (trace_id, apply_failed_append) in
        [("terminal-not-applied", false), ("terminal-applied-lost-ack", true)]
    {
        let backing = InMemoryPersistenceStore::new();
        let writes = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_store(FailOnAppend {
            backing: backing.clone(),
            calls: writes.clone(),
            fail_on_append: 3,
            apply_failed_append,
        }));
        let axon = Axon::<u32, u32, String, ()>::new("terminal-report-fixture")
            .then(Count(effects.clone()));

        let report = axon.execute(0, &(), &mut bus).await;

        assert!(matches!(
            &report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(1),
                operation: PersistenceOperation::Terminal,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ));
        assert!(!format!("{report:?}").contains("secret-backend-sentinel"));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(writes.load(Ordering::SeqCst), 3);

        let trace = backing.load(trace_id).await.unwrap().unwrap();
        // Neither a rejected append nor an applied-but-unacknowledged append
        // may certify completion to a fresh reader.
        assert_eq!(trace.completion, None);
        assert_eq!(trace.events.len(), if apply_failed_append { 3 } else { 2 });
        assert_eq!(
            trace
                .events
                .iter()
                .filter(|event| event.outcome_kind == "Next")
                .count(),
            if apply_failed_append { 2 } else { 1 }
        );
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        assert_eq!(
            bus.read::<PersistenceHandle>()
                .unwrap()
                .store()
                .load(trace_id)
                .await
                .unwrap(),
            Some(trace.clone())
        );
        let reentry = axon.execute(0, &(), &mut bus).await;
        assert!(matches!(
            reentry,
            ExecutionReport::BlockedBeforeEffect {
                operation: PersistenceOperation::Terminal,
                effect: EffectBoundary::NotInvoked,
                ..
            }
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(writes.load(Ordering::SeqCst), 3);
        assert_eq!(backing.load(trace_id).await.unwrap().unwrap(), trace);
    }
}

#[tokio::test]
async fn actual_execute_does_not_complete_when_cancelled_terminal_ack_is_unconfirmed() {
    use ranvier_core::cancellation::{CancellationReason, CancellationToken};
    for (trace_id, apply_failed_append) in [
        ("cancelled-terminal-not-applied", false),
        ("cancelled-terminal-applied-lost-ack", true),
    ] {
        let backing = InMemoryPersistenceStore::new();
        let writes = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_store(FailOnAppend {
            backing: backing.clone(),
            calls: writes.clone(),
            fail_on_append: 2,
            apply_failed_append,
        }));
        let axon = Axon::<u32, u32, String, ()>::new("cancelled-terminal-report")
            .then(Count(effects.clone()));
        let token = CancellationToken::new();
        assert!(token.cancel(CancellationReason::Explicit));

        let report = axon.execute_cancellable(0, &(), &mut bus, token).await;

        assert!(matches!(
            report,
            ExecutionReport::Unconfirmed {
                result: ranvier_runtime::ExecutionTerminal::Cancelled(_),
                operation: PersistenceOperation::Terminal,
                effect: EffectBoundary::NotInvoked,
                ..
            }
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(writes.load(Ordering::SeqCst), 2);
        let trace = backing.load(trace_id).await.unwrap().unwrap();
        assert_eq!(trace.completion, None);
        assert_eq!(trace.events.len(), if apply_failed_append { 2 } else { 1 });
        assert_eq!(trace.events[0].outcome_kind, "Enter");
        if apply_failed_append {
            assert_eq!(trace.events[1].outcome_kind, "Cancelled");
        }
    }
}

struct CompleteErrorStore {
    backing: InMemoryPersistenceStore,
    apply_before_error: bool,
    operations: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl PersistenceStore for CompleteErrorStore {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("append:{}", event.outcome_kind));
        self.backing.append(event).await
    }

    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        self.backing.load(trace).await
    }

    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        self.backing.resume(trace, step).await
    }

    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.operations.lock().unwrap().push("complete".into());
        if self.apply_before_error {
            self.backing.complete(trace, completion).await?;
        }
        Err(anyhow::anyhow!("private-complete-error-sentinel"))
    }

    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.backing.save_intervention(trace, intervention).await
    }
}

#[tokio::test]
async fn actual_execute_reports_completion_error_without_replaying_domain_effect() {
    for apply_before_error in [false, true] {
        let backing = InMemoryPersistenceStore::new();
        let store = Arc::new(CompleteErrorStore {
            backing: backing.clone(),
            apply_before_error,
            operations: std::sync::Mutex::new(Vec::new()),
        });
        let effects = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new("complete-error"));
        bus.insert(PersistenceHandle::from_arc(store.clone()));
        let axon = Axon::<u32, u32, String, ()>::new("complete-error").then(Count(effects.clone()));

        let report = axon.execute(0, &(), &mut bus).await;

        assert!(matches!(
            report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Next(1),
                operation: PersistenceOperation::Complete,
                effect: EffectBoundary::MayHaveRun,
                ..
            }
        ));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-complete-error-sentinel")
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(
            *store.operations.lock().unwrap(),
            vec!["append:Enter", "append:Next", "append:Next", "complete"]
        );

        let trace = backing.load("complete-error").await.unwrap().unwrap();
        assert_eq!(trace.events.len(), 3);
        assert_eq!(
            trace
                .events
                .iter()
                .map(|event| event.outcome_kind.as_str())
                .collect::<Vec<_>>(),
            vec!["Enter", "Next", "Next"]
        );
        assert!(trace.events[1].node_id.is_some());
        assert!(trace.events[2].node_id.is_none());
        assert_eq!(
            trace.completion,
            apply_before_error.then_some(CompletionState::Success)
        );
    }
}

#[tokio::test]
async fn actual_execute_reports_absent_persistence_and_unrequested_completion() {
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String, ()>::new("report-fixture").then(Count(effects.clone()));
    let mut unpersisted_bus = Bus::new();
    assert!(matches!(
        axon.execute(0, &(), &mut unpersisted_bus).await,
        ExecutionReport::Unpersisted {
            result: Outcome::Next(1),
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);

    let backing = InMemoryPersistenceStore::new();
    let store = Arc::new(CompleteErrorStore {
        backing: backing.clone(),
        apply_before_error: false,
        operations: std::sync::Mutex::new(Vec::new()),
    });
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("auto-complete-not-requested"));
    bus.insert(PersistenceHandle::from_arc(store.clone()));
    bus.insert(PersistenceAutoComplete(false));
    assert!(matches!(
        axon.execute(0, &(), &mut bus).await,
        ExecutionReport::Recorded {
            result: Outcome::Next(1),
            completion: CompletionConfirmation::NotRequested,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert_eq!(
        *store.operations.lock().unwrap(),
        vec!["append:Enter", "append:Next", "append:Next"]
    );

    let trace = backing
        .load("auto-complete-not-requested")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.events.len(), 3);
    assert_eq!(trace.completion, None);
}

#[derive(Clone)]
struct RecordUndo(std::sync::Arc<std::sync::Mutex<Vec<u32>>>);
#[async_trait::async_trait]
impl Transition<u32, ()> for RecordUndo {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: u32, _: &(), bus: &mut Bus) -> Outcome<(), String> {
        assert!(
            bus.cancellation_token()
                .is_some_and(|token| !token.is_cancelled())
        );
        self.0.lock().unwrap().push(input);
        Outcome::next(())
    }
}

#[tokio::test]
async fn compensated_result_ack_cancellation_retains_undo_and_uncertainty() {
    for apply_blocked in [false, true] {
        verify_compensated_result_ack_cancellation(apply_blocked).await;
    }
}

async fn verify_compensated_result_ack_cancellation(apply_blocked: bool) {
    use ranvier_core::cancellation::{CancellationReason, CancellationToken};
    use ranvier_core::saga::SagaPolicy;
    let backing = InMemoryPersistenceStore::new();
    let applied = Arc::new(tokio::sync::Notify::new());
    let effects = Arc::new(AtomicUsize::new(0));
    let later_effects = Arc::new(AtomicUsize::new(0));
    let undo_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let original: Arc<dyn PersistenceStore> = Arc::new(AppliedAwaitingAck {
        backing: backing.clone(),
        calls: AtomicUsize::new(0),
        block_at: 2,
        apply_blocked,
        applied: applied.clone(),
    });
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("compensated-result-ack"));
    bus.insert(PersistenceHandle::from_arc(original.clone()));
    let token = CancellationToken::new();
    let axon = Axon::<u32, u32, String>::new("compensated-result-ack")
        .with_saga_policy(SagaPolicy::Enabled)
        .then_compensated(Count(effects.clone()), RecordUndo(undo_inputs.clone()))
        .then(Count(later_effects.clone()));
    let report = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            axon.execute_cancellable(41, &(), &mut bus, token.clone()),
            async {
                applied.notified().await;
                assert!(token.cancel(CancellationReason::Explicit));
            }
        )
        .0
    })
    .await
    .unwrap();
    let owned_cleanup = report.cleanup().unwrap();
    assert!(owned_cleanup.is_complete());
    assert_eq!(owned_cleanup.steps.len(), 1);
    assert_eq!(owned_cleanup.trace_id, "compensated-result-ack");
    assert!(matches!(
        report,
        ExecutionReport::Unconfirmed {
            result: ranvier_runtime::ExecutionTerminal::Cancelled(_),
            operation: PersistenceOperation::Result,
            effect: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(later_effects.load(Ordering::SeqCst), 0);
    assert_eq!(*undo_inputs.lock().unwrap(), vec![41]);
    let trace = backing
        .load("compensated-result-ack")
        .await
        .unwrap()
        .unwrap();
    let result_events: Vec<_> = trace
        .events
        .iter()
        .filter(|event| event.outcome_kind == "Next")
        .collect();
    assert_eq!(result_events.len(), usize::from(apply_blocked));
    if apply_blocked {
        assert_eq!(
            result_events[0].payload,
            Some(Outcome::<u32, String>::Next(42).to_json_value())
        );
    }
    assert_eq!(trace.events.first().unwrap().outcome_kind, "Enter");
    assert_eq!(trace.events.last().unwrap().outcome_kind, "Cancelled");
    assert_eq!(trace.completion, Some(CompletionState::Cancelled));
    assert!(Arc::ptr_eq(
        &original,
        &bus.read::<PersistenceHandle>().unwrap().store()
    ));
}

struct MarkerWriteAckFailure {
    backing: InMemoryCompensationIdempotencyStore,
    apply_before_error: bool,
    reads: AtomicUsize,
    writes: AtomicUsize,
    last_key: std::sync::Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl CompensationIdempotencyStore for MarkerWriteAckFailure {
    async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        *self.last_key.lock().unwrap() = Some(key.to_string());
        self.backing.was_compensated(key).await
    }

    async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        *self.last_key.lock().unwrap() = Some(key.to_string());
        if self.apply_before_error {
            self.backing.mark_compensated(key).await?;
        }
        Err(anyhow::anyhow!("private-marker-ack-sentinel"))
    }
}

#[derive(Clone)]
struct CountFault(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Transition<u32, u32> for CountFault {
    type Error = String;
    type Resources = ();

    async fn run(&self, _: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::Fault("business-fault".into())
    }
}

#[derive(Clone)]
struct CountCompensation(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl CompensationHook for CountCompensation {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone)]
struct FailingCompensation(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl CompensationHook for FailingCompensation {
    async fn compensate(&self, _: CompensationContext) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("private-hook-error-sentinel"))
    }
}

#[tokio::test]
async fn compensation_marker_write_ack_loss_preserves_fault_and_uncertainty() {
    for apply_before_error in [false, true] {
        let trace_id = if apply_before_error {
            "marker-applied-ack-lost"
        } else {
            "marker-not-applied"
        };
        let trace_store = InMemoryPersistenceStore::new();
        let marker_store = Arc::new(MarkerWriteAckFailure {
            backing: InMemoryCompensationIdempotencyStore::new(),
            apply_before_error,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            last_key: std::sync::Mutex::new(None),
        });
        let effects = Arc::new(AtomicUsize::new(0));
        let compensation_calls = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(PersistenceHandle::from_store(trace_store.clone()));
        bus.insert(CompensationIdempotencyHandle::from_arc(
            marker_store.clone(),
        ));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            compensation_calls.clone(),
        )));
        let axon =
            Axon::<u32, u32, String, ()>::new("marker-ack-loss").then(CountFault(effects.clone()));

        let report = axon.execute(7, &(), &mut bus).await;

        assert!(matches!(
            report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Fault(ref message),
                operation: PersistenceOperation::CompensationWrite,
                effect: EffectBoundary::MayHaveRun,
                ..
            } if message == "business-fault"
        ));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-marker-ack-sentinel")
        );
        let cleanup = report.cleanup().expect("external compensation is observed");
        assert_eq!(cleanup.trace_id, trace_id);
        assert!(!cleanup.is_complete());
        assert_eq!(cleanup.steps.len(), 1);
        assert_eq!(
            cleanup.steps[0].outcome,
            ranvier_runtime::SagaRollbackOutcome::Fault
        );
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(compensation_calls.load(Ordering::SeqCst), 1);
        assert_eq!(marker_store.reads.load(Ordering::SeqCst), 1);
        assert_eq!(marker_store.writes.load(Ordering::SeqCst), 1);

        let key = marker_store.last_key.lock().unwrap().clone().unwrap();
        assert_eq!(
            marker_store.backing.was_compensated(&key).await.unwrap(),
            apply_before_error
        );
        let trace = trace_store.load(trace_id).await.unwrap().unwrap();
        assert_eq!(trace.completion, Some(CompletionState::Fault));
        assert_eq!(
            trace
                .events
                .iter()
                .map(|event| event.outcome_kind.as_str())
                .collect::<Vec<_>>(),
            vec!["Enter", "Fault", "Fault"]
        );
    }
}

#[tokio::test]
async fn compensation_marker_write_does_not_claim_execution_trace_recorded() {
    let idempotency = InMemoryCompensationIdempotencyStore::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let compensation_calls = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("trace-marker-only"));
    bus.insert(CompensationIdempotencyHandle::from_store(
        idempotency.clone(),
    ));
    bus.insert(CompensationHandle::from_hook(CountCompensation(
        compensation_calls.clone(),
    )));
    let axon = Axon::<u32, u32, String, ()>::new("compensation-marker-only")
        .then(CountFault(effects.clone()));

    let report = axon.execute(7, &(), &mut bus).await;

    let cleanup = report.cleanup().expect("external compensation is observed");
    assert!(cleanup.is_complete());
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(compensation_calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        report,
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(ref message),
            ..
        } if message == "business-fault"
    ));
    let marker_key = "trace-marker-only:compensation-marker-only:Fault";
    assert!(idempotency.was_compensated(marker_key).await.unwrap());
}

#[tokio::test]
async fn compensation_hook_failure_without_trace_store_keeps_cleanup_incomplete() {
    let marker_store = Arc::new(MarkerWriteAckFailure {
        backing: InMemoryCompensationIdempotencyStore::new(),
        apply_before_error: false,
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
        last_key: std::sync::Mutex::new(None),
    });
    let effects = Arc::new(AtomicUsize::new(0));
    let compensation_calls = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("hook-failure-without-trace"));
    bus.insert(CompensationIdempotencyHandle::from_arc(
        marker_store.clone(),
    ));
    bus.insert(CompensationHandle::from_hook(FailingCompensation(
        compensation_calls.clone(),
    )));
    let axon = Axon::<u32, u32, String, ()>::new("compensation-hook-failure")
        .then(CountFault(effects.clone()));

    let report = axon.execute(7, &(), &mut bus).await;

    assert!(matches!(
        report,
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(ref message),
            ..
        } if message == "business-fault"
    ));
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("private-hook-error-sentinel"));
    let cleanup = report.cleanup().expect("external compensation is observed");
    assert!(!cleanup.is_complete());
    assert_eq!(cleanup.trace_id, "hook-failure-without-trace");
    assert_eq!(cleanup.steps.len(), 1);
    assert_eq!(
        cleanup.steps[0].outcome,
        ranvier_runtime::SagaRollbackOutcome::Fault
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(compensation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(marker_store.reads.load(Ordering::SeqCst), 1);
    assert_eq!(marker_store.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn compensation_marker_ack_loss_without_trace_store_stays_unconfirmed() {
    for apply_before_error in [false, true] {
        let trace_id = if apply_before_error {
            "marker-only-applied-ack-lost"
        } else {
            "marker-only-not-applied"
        };
        let marker_store = Arc::new(MarkerWriteAckFailure {
            backing: InMemoryCompensationIdempotencyStore::new(),
            apply_before_error,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            last_key: std::sync::Mutex::new(None),
        });
        let effects = Arc::new(AtomicUsize::new(0));
        let compensation_calls = Arc::new(AtomicUsize::new(0));
        let mut bus = Bus::new();
        bus.insert(PersistenceTraceId::new(trace_id));
        bus.insert(CompensationIdempotencyHandle::from_arc(
            marker_store.clone(),
        ));
        bus.insert(CompensationHandle::from_hook(CountCompensation(
            compensation_calls.clone(),
        )));
        let axon = Axon::<u32, u32, String, ()>::new("compensation-marker-only")
            .then(CountFault(effects.clone()));

        let report = axon.execute(7, &(), &mut bus).await;

        assert!(matches!(
            report,
            ExecutionReport::Unconfirmed {
                result: Outcome::Fault(ref message),
                operation: PersistenceOperation::CompensationWrite,
                effect: EffectBoundary::MayHaveRun,
                ..
            } if message == "business-fault"
        ));
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-marker-ack-sentinel")
        );
        let cleanup = report.cleanup().unwrap();
        assert!(!cleanup.is_complete());
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(compensation_calls.load(Ordering::SeqCst), 1);
        assert_eq!(marker_store.reads.load(Ordering::SeqCst), 1);
        assert_eq!(marker_store.writes.load(Ordering::SeqCst), 1);
        let key = marker_store.last_key.lock().unwrap().clone().unwrap();
        assert_eq!(
            marker_store.backing.was_compensated(&key).await.unwrap(),
            apply_before_error
        );
    }
}
