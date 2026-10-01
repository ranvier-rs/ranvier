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
    fn report<T>(&self, result: T) -> ExecutionReport<T> {
        let operation = *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(operation) = operation {
            return ExecutionReport::Unconfirmed {
                result,
                operation,
                effect: if self.effect.load(Ordering::SeqCst) {
                    EffectBoundary::MayHaveRun
                } else {
                    EffectBoundary::NotInvoked
                },
            };
        }
        if !self.attempted_write.load(Ordering::SeqCst) {
            return ExecutionReport::NotRequested { result };
        }
        ExecutionReport::Recorded {
            result,
            completion: if self.completed.load(Ordering::SeqCst) {
                CompletionConfirmation::Confirmed
            } else {
                CompletionConfirmation::NotRequested
            },
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

pub(crate) struct Session {
    original: Option<PersistenceHandle>,
    original_compensation: Option<CompensationIdempotencyHandle>,
    previous: Option<ActiveObservation>,
    observation: Arc<Observation>,
}
impl Session {
    pub(crate) fn begin(bus: &mut Bus) -> Self {
        let original = bus.read::<PersistenceHandle>().cloned();
        let original_compensation = bus.read::<CompensationIdempotencyHandle>().cloned();
        let previous = bus.read::<ActiveObservation>().cloned();
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
            observation,
        }
    }
    pub(crate) fn finish<T>(self, bus: &mut Bus, result: T) -> ExecutionReport<T> {
        bus.clear_access_policy();
        let report = if self.original.is_some() || self.original_compensation.is_some() {
            self.observation.report(result)
        } else {
            ExecutionReport::Unpersisted { result }
        };
        if let Some(original) = self.original {
            bus.insert(original);
        }
        if let Some(original) = self.original_compensation {
            bus.insert(original);
        }
        if let Some(previous) = self.previous {
            bus.insert(previous);
        } else {
            let _ = bus.remove::<ActiveObservation>();
        }
        report
    }
}
