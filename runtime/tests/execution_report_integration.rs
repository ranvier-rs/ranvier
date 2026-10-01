use ranvier_core::{Bus, Outcome, Transition};
use ranvier_runtime::persistence::*;
use ranvier_runtime::{
    Axon, CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct AppliedAwaitingAck {
    backing: InMemoryPersistenceStore,
    calls: AtomicUsize,
    block_at: usize,
    applied: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl PersistenceStore for AppliedAwaitingAck {
    async fn append(&self, event: PersistenceEnvelope) -> anyhow::Result<()> {
        self.backing.append(event).await?;
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.block_at {
            self.applied.notify_one();
            std::future::pending::<()>().await;
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
        } if operation == expected_operation && effect == expected_effect
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
            effect: EffectBoundary::MayHaveRun
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
            result: Outcome::Next(1)
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
            completion: CompletionConfirmation::NotRequested
        }
    ));
}
