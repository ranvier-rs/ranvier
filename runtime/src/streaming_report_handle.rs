//! Typed report custody for supervised streams. A report lease is not a
//! reconciliation decision; keep it until pending effects are resolved explicitly.
use std::sync::{Arc, atomic::{AtomicU8, Ordering}};
use tokio::time::Instant;
use crate::{StreamInitializationReport, StreamConsumptionTerminal};

type Original<Out, E> = Result<StreamInitializationReport<Out,
    crate::streaming_recovery::StreamReadyResult<()>, E>,
    crate::streaming_recovery::StreamInvocationAlreadyStarted>;

/// Late consumption evidence, separate from prefix durability and cleanup.
pub struct SupervisedStreamReady {
    pub consumption: Result<StreamConsumptionTerminal, SupervisedStreamFailure>,
    pub cleanup: Option<crate::SagaRollbackReport>,
}
pub enum SupervisedStreamFailure {
    DriverPanicked,
    Cancelled(ranvier_core::CancellationContext),
}
/// Original typed reports and actual JoinError are transferred without cloning,
/// serialization or Debug. Raw initializer errors are private application detail.
pub enum SupervisedStreamCompletion<Out, E> {
    Report(StreamInitializationReport<Out, SupervisedStreamReady, E>),
    RejectedBeforeStart,
    InvocationAlreadyStarted,
    FrameFailed(tokio::task::JoinError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReportWaitError { TimedOut, AlreadyTaken }

/// Cached evidence from the exact joined frame, retained after report transfer.
/// Every executed or unknown frame still requires recovery review; EOF is not
/// authority to release resources or resolve external effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRetirementEvidence { NotObserved, RejectedBeforeStart, RequiresRecoveryReview }

/// Clones share one original report. Waiting never retires recovery custody;
/// only explicit report transfer consumes the result, once across all clones.
/// Dropping the final lease is not successful cleanup or durable reconciliation.
pub struct StreamReportHandle<Out, E> {
    task: Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>>>,
    _owner: crate::nested_custody::Capability,
    retirement: Arc<AtomicU8>,
}
impl<Out, E> Clone for StreamReportHandle<Out, E> {
    fn clone(&self) -> Self { Self { task: self.task.clone(), _owner: self._owner.clone(), retirement: self.retirement.clone() } }
}
impl<Out: Send + 'static, E: Send + 'static> StreamReportHandle<Out, E> {
    pub(crate) fn new(task: Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>>>,
        owner: crate::nested_custody::Capability) -> Self { Self { task, _owner: owner, retirement: Arc::new(AtomicU8::new(0)) } }
    /// Join without taking the report. Timeout or dropped wait keeps the same
    /// frame/result available, including during mutex contention.
    pub async fn settle_until(&self, deadline: Instant) -> Result<(), StreamReportWaitError> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock()).await
            .map_err(|_| StreamReportWaitError::TimedOut)?;
        task.settle_until(deadline).await.map_err(wait_error)?;
        self.observe_retirement(&task);
        Ok(())
    }
    pub fn retirement_evidence(&self) -> StreamRetirementEvidence {
        match self.retirement.load(Ordering::Acquire) {
            1 => StreamRetirementEvidence::RejectedBeforeStart,
            2 => StreamRetirementEvidence::RequiresRecoveryReview,
            _ => StreamRetirementEvidence::NotObserved,
        }
    }
    fn observe_retirement(&self, task: &crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>) {
        if let Some(rejected) = task.rejected_before_start() {
            self.retirement.store(if rejected { 1 } else { 2 }, Ordering::Release);
        }
    }
    pub(crate) async fn prove_rejected_before_start(&self, deadline: Instant) -> Result<bool, StreamReportWaitError> {
        if self.retirement_evidence() == StreamRetirementEvidence::NotObserved {
            // A transferred report may already have populated the cache while
            // this waiter contended for the lock. Recheck after AlreadyTaken.
            if let Err(error) = self.settle_until(deadline).await {
                if error != StreamReportWaitError::AlreadyTaken || self.retirement_evidence() == StreamRetirementEvidence::NotObserved {
                    return Err(error);
                }
            }
        }
        Ok(self.retirement_evidence() == StreamRetirementEvidence::RejectedBeforeStart)
    }
    /// Explicit single transfer of the original result; readiness never does this.
    pub async fn take_report_until(&self, deadline: Instant)
        -> Result<SupervisedStreamCompletion<Out, E>, StreamReportWaitError> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock()).await
            .map_err(|_| StreamReportWaitError::TimedOut)?;
        task.settle_until(deadline).await.map_err(wait_error)?;
        self.observe_retirement(&task);
        let joined = task.take_joined().ok_or(StreamReportWaitError::AlreadyTaken)?;
        Ok(match joined {
            Err(error) => SupervisedStreamCompletion::FrameFailed(error),
            Ok(None) => SupervisedStreamCompletion::RejectedBeforeStart,
            Ok(Some(Err(_))) => SupervisedStreamCompletion::InvocationAlreadyStarted,
            Ok(Some(Ok(report))) => SupervisedStreamCompletion::Report(report.map_value(|ready| {
                SupervisedStreamReady {
                    consumption: ready.consumption.map(|consumed| consumed.terminal).map_err(|error| match error {
                        crate::streaming_recovery::StreamDriverError::Panicked => SupervisedStreamFailure::DriverPanicked,
                        crate::streaming_recovery::StreamDriverError::Cancelled(context) => SupervisedStreamFailure::Cancelled(context),
                    }),
                    cleanup: ready.cleanup,
                }
            })),
        })
    }
}
fn wait_error(error: crate::managed_stream_task::ManagedTaskWaitError) -> StreamReportWaitError {
    match error {
        crate::managed_stream_task::ManagedTaskWaitError::TimedOut => StreamReportWaitError::TimedOut,
        _ => StreamReportWaitError::AlreadyTaken,
    }
}
