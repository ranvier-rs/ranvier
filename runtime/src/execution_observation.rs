use crate::execution_report::{
    CompletionConfirmation, EffectBoundary, ExecutionReport, PersistenceOperation,
};
use crate::persistence::*;
use ranvier_core::Bus;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub(crate) struct Observation {
    failure: Mutex<Option<PersistenceOperation>>,
    effect: AtomicBool,
    attempted_write: AtomicBool,
    completed: AtomicBool,
    blocked: AtomicBool,
    encoding_failed: AtomicBool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    struct AppliedWithoutAck {
        backing: InMemoryCompensationIdempotencyStore,
        pending: bool,
    }
    #[async_trait::async_trait]
    impl CompensationIdempotencyStore for AppliedWithoutAck {
        async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
            self.backing.was_compensated(key).await
        }
        async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
            self.backing.mark_compensated(key).await?;
            if self.pending {
                std::future::pending::<()>().await;
            }
            Err(anyhow::anyhow!("private-error-sentinel"))
        }
    }

    async fn check_applied_write(pending: bool) {
        let backing = InMemoryCompensationIdempotencyStore::new();
        let mut bus = Bus::new();
        bus.insert(CompensationIdempotencyHandle::from_store(
            AppliedWithoutAck {
                backing: backing.clone(),
                pending,
            },
        ));
        let original = bus.read::<CompensationIdempotencyHandle>().unwrap().store();
        let session = Session::begin(&mut bus);
        session.observation.effect();
        let store = bus.read::<CompensationIdempotencyHandle>().unwrap().store();
        assert!(!store.was_compensated("key").await.unwrap());
        if pending {
            // Poll through the applied write once, then drop before any ACK.
            assert!(store.mark_compensated("key").now_or_never().is_none());
        } else {
            assert!(store.mark_compensated("key").await.is_err());
        }
        let report = session.finish(&mut bus, "domain-result");
        assert_eq!(
            report,
            ExecutionReport::Unconfirmed {
                result: "domain-result",
                operation: PersistenceOperation::CompensationWrite,
                effect: EffectBoundary::MayHaveRun,
                cleanup: None,
            }
        );
        assert!(backing.was_compensated("key").await.unwrap());
        let restored = bus.read::<CompensationIdempotencyHandle>().unwrap();
        assert!(Arc::ptr_eq(&restored.store(), &original));
        assert!(restored.store().was_compensated("key").await.unwrap());
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-error-sentinel")
        );
    }

    #[test]
    fn nested_session_restores_parent_cleanup_journal() {
        use crate::saga_report::{SagaRollbackJournal, SagaRollbackOutcome, SagaRollbackReport};
        let mut bus = Bus::new();
        let outer = Session::begin(&mut bus);
        let parent = SagaRollbackJournal::new("parent");
        parent.push("parent-node".into(), SagaRollbackOutcome::InProgress);
        bus.insert(parent.clone());
        let parent_snapshot = parent.snapshot();
        bus.insert(parent_snapshot.clone());
        let inner = Session::begin(&mut bus);
        assert!(bus.read::<SagaRollbackJournal>().is_none());
        let child = SagaRollbackJournal::new("child");
        child.push("child-node".into(), SagaRollbackOutcome::Fault);
        child.finish();
        bus.insert(child.snapshot());
        bus.insert(child);
        let inner_result = inner.finish(&mut bus, "child-result");
        let child_cleanup = inner_result.cleanup().unwrap();
        assert_eq!(child_cleanup.trace_id, "child");
        assert_eq!(child_cleanup.steps[0].outcome, SagaRollbackOutcome::Fault);
        assert!(!child_cleanup.is_complete());
        assert_eq!(bus.read::<SagaRollbackReport>().unwrap(), &parent_snapshot);
        parent.settle(SagaRollbackOutcome::Completed);
        assert_eq!(
            bus.read::<SagaRollbackJournal>().unwrap().snapshot(),
            parent.snapshot()
        );
        assert_eq!(
            bus.read::<SagaRollbackJournal>()
                .unwrap()
                .snapshot()
                .trace_id,
            "parent"
        );
        let outer_result = outer.finish(&mut bus, "parent-result");
        assert_eq!(outer_result.cleanup().unwrap().trace_id, "parent");
        assert_eq!(inner_result.cleanup().unwrap().trace_id, "child");
    }

    #[tokio::test]
    async fn applied_compensation_write_error_preserves_readback_and_report() {
        check_applied_write(false).await;
    }

    #[tokio::test]
    async fn dropped_compensation_write_preserves_readback_and_report() {
        check_applied_write(true).await;
    }
}

impl Observation {
    pub(crate) fn encoding_failed(&self, operation: PersistenceOperation) {
        self.encoding_failed.store(true, Ordering::SeqCst);
        self.failed(operation);
    }
    pub(crate) fn has_encoding_failure(&self) -> bool {
        self.encoding_failed.load(Ordering::SeqCst)
    }
    fn failed(&self, operation: PersistenceOperation) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failure.is_none() {
            *failure = Some(operation);
        }
    }
    fn observe<T>(&self, operation: PersistenceOperation, result: &anyhow::Result<T>) {
        if result.is_err() {
            self.failed(operation);
        }
    }
    pub(crate) fn effect(&self) {
        self.effect.store(true, Ordering::SeqCst);
    }
    pub(crate) fn block(&self, operation: PersistenceOperation) {
        self.failed(operation);
        self.blocked.store(true, Ordering::SeqCst);
    }
    fn report<T>(&self, result: T) -> ExecutionReport<T> {
        let operation = *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(operation) = operation {
            if self.blocked.load(Ordering::SeqCst) {
                return ExecutionReport::BlockedBeforeEffect {
                    operation,
                    effect: if self.effect.load(Ordering::SeqCst) {
                        EffectBoundary::MayHaveRun
                    } else {
                        EffectBoundary::NotInvoked
                    },
                    cleanup: None,
                };
            }
            return ExecutionReport::Unconfirmed {
                result,
                operation,
                effect: if self.effect.load(Ordering::SeqCst) {
                    EffectBoundary::MayHaveRun
                } else {
                    EffectBoundary::NotInvoked
                },
                cleanup: None,
            };
        }
        if !self.attempted_write.load(Ordering::SeqCst) {
            return ExecutionReport::NotRequested {
                result,
                cleanup: None,
            };
        }
        ExecutionReport::Recorded {
            result,
            completion: if self.completed.load(Ordering::SeqCst) {
                CompletionConfirmation::Confirmed
            } else {
                CompletionConfirmation::NotRequested
            },
            cleanup: None,
        }
    }
}

// Dropping a polled store future cannot establish non-commit or acknowledgement.
struct ObservedCall<'a> {
    observation: &'a Observation,
    operation: PersistenceOperation,
    settled: bool,
}
impl<'a> ObservedCall<'a> {
    fn new(observation: &'a Observation, operation: PersistenceOperation) -> Self {
        Self {
            observation,
            operation,
            settled: false,
        }
    }
    fn finish<T>(mut self, result: &anyhow::Result<T>) {
        self.observation.observe(self.operation, result);
        self.settled = true;
    }
}
impl Drop for ObservedCall<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.observation.failed(self.operation);
        }
    }
}

pub(crate) struct ObservedStore {
    pub(crate) inner: Arc<dyn PersistenceStore>,
    pub(crate) observation: Arc<Observation>,
}
#[async_trait::async_trait]
impl PersistenceStore for ObservedStore {
    async fn append(&self, envelope: PersistenceEnvelope) -> anyhow::Result<()> {
        let operation = if envelope.outcome_kind == "Enter" {
            PersistenceOperation::Intent
        } else if envelope.node_id.is_none() {
            PersistenceOperation::Terminal
        } else {
            PersistenceOperation::Result
        };
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, operation);
        let result = self.inner.append(envelope).await;
        call.finish(&result);
        result
    }
    async fn load(&self, trace: &str) -> anyhow::Result<Option<PersistedTrace>> {
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Load);
        let result = self.inner.load(trace).await;
        call.finish(&result);
        result
    }
    async fn resume(&self, trace: &str, step: u64) -> anyhow::Result<ResumeCursor> {
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Load);
        let result = self.inner.resume(trace, step).await;
        call.finish(&result);
        result
    }
    async fn complete(&self, trace: &str, completion: CompletionState) -> anyhow::Result<()> {
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Complete);
        let result = self.inner.complete(trace, completion).await;
        call.finish(&result);
        if result.is_ok() {
            self.observation.completed.store(true, Ordering::SeqCst);
        }
        result
    }
    async fn save_intervention(
        &self,
        trace: &str,
        intervention: Intervention,
    ) -> anyhow::Result<()> {
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, PersistenceOperation::Result);
        let result = self.inner.save_intervention(trace, intervention).await;
        call.finish(&result);
        result
    }
}

#[derive(Clone)]
pub(crate) struct ActiveObservation(pub(crate) Arc<Observation>);

pub(crate) struct ObservedCompensationStore {
    pub(crate) inner: Arc<dyn CompensationIdempotencyStore>,
    pub(crate) observation: Arc<Observation>,
}
#[async_trait::async_trait]
impl CompensationIdempotencyStore for ObservedCompensationStore {
    async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
        let call = ObservedCall::new(&self.observation, PersistenceOperation::CompensationRead);
        let result = self.inner.was_compensated(key).await;
        call.finish(&result);
        result
    }
    async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
        self.observation
            .attempted_write
            .store(true, Ordering::SeqCst);
        let call = ObservedCall::new(&self.observation, PersistenceOperation::CompensationWrite);
        let result = self.inner.mark_compensated(key).await;
        call.finish(&result);
        result
    }
}

#[derive(Clone)]
pub(crate) struct InvocationTrace(pub(crate) String);

#[cfg(test)]
mod invocation_trace_tests {
    use super::{InvocationTrace, Session};
    use ranvier_core::Bus;

    #[test]
    fn invocation_identity_is_stable_unique_and_restored_after_nested_finish() {
        let mut bus = Bus::new();
        let parent = Session::begin(&mut bus);
        let parent_id = bus.read::<InvocationTrace>().unwrap().0.clone();
        assert_eq!(bus.read::<InvocationTrace>().unwrap().0, parent_id);
        let child = Session::begin(&mut bus);
        let child_id = bus.read::<InvocationTrace>().unwrap().0.clone();
        assert_ne!(parent_id, child_id);
        let _ = child.finish(&mut bus, ());
        assert_eq!(bus.read::<InvocationTrace>().unwrap().0, parent_id);
        let _ = parent.finish(&mut bus, ());
        assert!(bus.read::<InvocationTrace>().is_none());
        let next = Session::begin(&mut bus);
        assert_ne!(bus.read::<InvocationTrace>().unwrap().0, parent_id);
        let _ = next.finish(&mut bus, ());
        assert!(bus.read::<InvocationTrace>().is_none());
    }
}

pub(crate) struct Session {
    original: Option<PersistenceHandle>,
    original_compensation: Option<CompensationIdempotencyHandle>,
    previous: Option<ActiveObservation>,
    previous_trace: Option<InvocationTrace>,
    previous_cleanup: Option<crate::saga_report::SagaRollbackJournal>,
    previous_cleanup_report: Option<crate::saga_report::SagaRollbackReport>,
    observation: Arc<Observation>,
}
impl Session {
    pub(crate) fn begin(bus: &mut Bus) -> Self {
        let original = bus.read::<PersistenceHandle>().cloned();
        let original_compensation = bus.read::<CompensationIdempotencyHandle>().cloned();
        let previous = bus.read::<ActiveObservation>().cloned();
        let previous_trace = bus.remove::<InvocationTrace>();
        bus.insert(InvocationTrace(format!(
            "{}:{}",
            bus.id,
            uuid::Uuid::new_v4()
        )));
        let cleanup_report = bus.remove::<crate::saga_report::SagaRollbackReport>();
        let cleanup = bus.remove::<crate::saga_report::SagaRollbackJournal>();
        let (previous_cleanup, previous_cleanup_report) = if previous.is_some() {
            (cleanup, cleanup_report)
        } else {
            (None, None)
        };
        let observation = Arc::new(Observation::default());
        if let Some(handle) = &original {
            bus.insert(handle.observed(observation.clone()));
        }
        if let Some(handle) = &original_compensation {
            bus.insert(handle.observed(observation.clone()));
        }
        bus.insert(ActiveObservation(observation.clone()));
        Self {
            original,
            original_compensation,
            previous,
            previous_trace,
            previous_cleanup,
            previous_cleanup_report,
            observation,
        }
    }
    pub(crate) fn finish<T>(self, bus: &mut Bus, result: T) -> ExecutionReport<T> {
        let report = self.build_report(bus, result);
        self.restore(bus);
        report
    }

    fn build_report<T>(&self, bus: &mut Bus, result: T) -> ExecutionReport<T> {
        bus.clear_access_policy();
        // Capture child ownership before restoring a parent's Bus observations.
        let cleanup = bus
            .read::<crate::saga_report::SagaRollbackJournal>()
            .map(|journal| journal.snapshot())
            .or_else(|| {
                bus.read::<crate::saga_report::SagaRollbackReport>()
                    .cloned()
            });
        let report = if self.original.is_some() || self.original_compensation.is_some() {
            self.observation.report(result)
        } else {
            ExecutionReport::Unpersisted {
                result,
                cleanup: None,
            }
        }
        .with_cleanup(cleanup);
        report
    }

    fn restore(self, bus: &mut Bus) {
        bus.clear_access_policy();
        if let Some(original) = self.original {
            bus.insert(original);
        }
        if let Some(original) = self.original_compensation {
            bus.insert(original);
        }
        let _ = bus.remove::<InvocationTrace>();
        if let Some(trace) = self.previous_trace {
            bus.insert(trace);
        }
        if let Some(previous) = self.previous {
            bus.insert(previous);
            let _ = bus.remove::<crate::saga_report::SagaRollbackJournal>();
            let _ = bus.remove::<crate::saga_report::SagaRollbackReport>();
            if let Some(journal) = self.previous_cleanup {
                bus.insert(journal);
            }
            if let Some(report) = self.previous_cleanup_report {
                bus.insert(report);
            }
        } else {
            let _ = bus.remove::<ActiveObservation>();
        }
    }
}

/// Owns the Bus borrow until the executor future and its child borrows are dropped.
pub(crate) struct InvocationOwner<'a> {
    bus: &'a mut Bus,
    session: Option<Session>,
    token: Option<ranvier_core::cancellation::CancellationToken>,
}

impl<'a> InvocationOwner<'a> {
    pub(crate) fn begin(bus: &'a mut Bus) -> Self {
        let token = bus.cancellation_token().cloned();
        let session = Session::begin(bus);
        Self {
            bus,
            session: Some(session),
            token,
        }
    }
    pub(crate) fn bus(&mut self) -> &mut Bus {
        self.bus
    }
    pub(crate) fn finish<T>(mut self, result: T) -> ExecutionReport<T> {
        let report = match self.session.as_ref() {
            Some(session) => session.build_report(self.bus, result),
            None => unreachable!("invocation session is consumed only by finish"),
        };
        if let Some(session) = self.session.take() {
            session.restore(self.bus);
        }
        report
    }
}

impl Drop for InvocationOwner<'_> {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.restore(self.bus);
        }
        self.bus.replace_cancellation_token(self.token.take());
    }
}
