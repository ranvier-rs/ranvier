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

pub(crate) struct Session {
    original: Option<PersistenceHandle>,
    previous: Option<ActiveObservation>,
    observation: Arc<Observation>,
}
impl Session {
    pub(crate) fn begin(bus: &mut Bus) -> Self {
        let original = bus.read::<PersistenceHandle>().cloned();
        let previous = bus.read::<ActiveObservation>().cloned();
        let observation = Arc::new(Observation::default());
        if let Some(handle) = &original {
            bus.insert(handle.observed(observation.clone()));
        }
        bus.insert(ActiveObservation(observation.clone()));
        Self {
            original,
            previous,
            observation,
        }
    }
    pub(crate) fn finish<T>(self, bus: &mut Bus, result: T) -> ExecutionReport<T> {
        bus.clear_access_policy();
        let report = if self.original.is_some() {
            self.observation.report(result)
        } else {
            ExecutionReport::Unpersisted { result }
        };
        if let Some(original) = self.original {
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
