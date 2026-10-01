use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{
    Axon, CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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
#[async_trait::async_trait]
impl Transition<u32, u32> for Count {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: u32, _: &(), _: &mut Bus) -> Outcome<u32, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Outcome::next(input + 1)
    }
}

struct FailSecondAppend {
    backing: InMemoryPersistenceStore,
    calls: Arc<AtomicUsize>,
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
async fn collected_stream_terminal_failure_retains_invoked_effect_boundary() {
    let backing = InMemoryPersistenceStore::new();
    let writes = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("stream-report"));
    bus.insert(PersistenceHandle::from_store(FailSecondAppend {
        backing: backing.clone(),
        calls: writes.clone(),
    }));
    let axon = Axon::<u32, u32, String, ()>::new("stream-report")
        .then_stream(CountStream(effects.clone()))
        .collect_into_vec();
    let report = axon.execute(7, &(), &mut bus).await;
    assert!(matches!(report, ExecutionReport::Unconfirmed {
        result: Outcome::Next(ref items), operation: PersistenceOperation::Terminal,
        effect: EffectBoundary::MayHaveRun,
     .. } if items == &[7, 8]));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 2);
    let trace = backing.load("stream-report").await.unwrap().unwrap();
    assert_eq!(trace.events.len(), 1);
    assert_eq!(trace.events[0].outcome_kind, "Enter");
    assert_eq!(trace.completion, Some(CompletionState::Success));
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
impl PersistenceStore for FailSecondAppend {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
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
async fn actual_execute_retains_first_failure_after_terminal_and_completion_succeed() {
    let backing = InMemoryPersistenceStore::new();
    let writes = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let mut bus = Bus::new();
    bus.insert(PersistenceTraceId::new("observed"));
    bus.insert(PersistenceHandle::from_store(FailSecondAppend {
        backing: backing.clone(),
        calls: writes.clone(),
    }));
    let axon = Axon::<u32, u32, String, ()>::new("report-fixture").then(Count(effects.clone()));
    let report = axon.execute(0, &(), &mut bus).await;
    assert!(matches!(
        report,
        ExecutionReport::Unconfirmed {
            result: Outcome::Next(1),
            operation: PersistenceOperation::Result,
            effect: EffectBoundary::MayHaveRun,
            ..
        }
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(writes.load(Ordering::SeqCst), 3);
    let trace = backing.load("observed").await.unwrap().unwrap();
    assert_eq!(trace.completion, Some(CompletionState::Success));
    assert_eq!(trace.events.len(), 2);
    assert_eq!(
        bus.read::<PersistenceHandle>()
            .unwrap()
            .store()
            .load("observed")
            .await
            .unwrap(),
        Some(trace)
    );
}

#[tokio::test]
async fn actual_execute_distinguishes_no_store_and_app_owned_completion() {
    let effects = Arc::new(AtomicUsize::new(0));
    let axon = Axon::<u32, u32, String, ()>::new("report-fixture").then(Count(effects));
    let mut bus = Bus::new();
    assert!(matches!(
        axon.execute(0, &(), &mut bus).await,
        ExecutionReport::Unpersisted {
            result: Outcome::Next(1),
            ..
        }
    ));
    bus.insert(PersistenceHandle::from_store(
        InMemoryPersistenceStore::new(),
    ));
    bus.insert(PersistenceAutoComplete(false));
    assert!(matches!(
        axon.execute(0, &(), &mut bus).await,
        ExecutionReport::Recorded {
            result: Outcome::Next(1),
            completion: CompletionConfirmation::NotRequested,
            ..
        }
    ));
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
