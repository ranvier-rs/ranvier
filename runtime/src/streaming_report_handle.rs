//! Typed report custody for supervised streams. A report lease is not a
//! reconciliation decision; keep it until pending effects are resolved explicitly.
use std::sync::Arc;
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

/// Clones share one original report. Waiting never retires recovery custody;
/// only explicit report transfer consumes the result, once across all clones.
/// Dropping the final lease is not successful cleanup or durable reconciliation.
pub struct StreamReportHandle<Out, E> {
    task: Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>>>,
    _owner: crate::nested_custody::Capability,
}
impl<Out, E> Clone for StreamReportHandle<Out, E> {
    fn clone(&self) -> Self { Self { task: self.task.clone(), _owner: self._owner.clone() } }
}
impl<Out: Send + 'static, E: Send + 'static> StreamReportHandle<Out, E> {
    pub(crate) fn new(task: Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>>>,
        owner: crate::nested_custody::Capability) -> Self { Self { task, _owner: owner } }
    /// Join without taking the report. Timeout or dropped wait keeps the same
    /// frame/result available, including during mutex contention.
    pub async fn settle_until(&self, deadline: Instant) -> Result<(), StreamReportWaitError> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock()).await
            .map_err(|_| StreamReportWaitError::TimedOut)?;
        task.settle_until(deadline).await.map_err(wait_error)
    }
    /// Explicit single transfer of the original result; readiness never does this.
    pub async fn take_report_until(&self, deadline: Instant)
        -> Result<SupervisedStreamCompletion<Out, E>, StreamReportWaitError> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock()).await
            .map_err(|_| StreamReportWaitError::TimedOut)?;
        task.settle_until(deadline).await.map_err(wait_error)?;
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
