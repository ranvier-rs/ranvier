//! Typed report custody for supervised streams. A report lease is not a
//! reconciliation decision; keep it until pending effects are resolved explicitly.
use crate::{StreamConsumptionTerminal, StreamInitializationReport};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
use tokio::time::Instant;

type Original<Out, E> = Result<
    StreamInitializationReport<Out, crate::streaming_recovery::StreamReadyResult<()>, E>,
    crate::streaming_recovery::StreamInvocationAlreadyStarted,
>;

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
pub enum StreamReportWaitError {
    TimedOut,
    AlreadyTaken,
}

/// Cached evidence from the exact joined frame, retained after report transfer.
/// Every executed or unknown frame still requires recovery review; EOF is not
/// authority to release resources or resolve external effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRetirementEvidence {
    NotObserved,
    RejectedBeforeStart,
    RequiresRecoveryReview,
}

/// Fixed observations from the original joined report; no payload/error strings.
/// Prefix durability never certifies terminal stream or external effect completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFrameAssessment {
    RejectedBeforeStart,
    Reported,
    AlreadyStarted,
    FrameFailed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPrefixDurability {
    NotObserved,
    Unpersisted,
    NotRequested,
    RecordedCompletionConfirmed,
    RecordedWithoutCompletion,
    Unconfirmed,
    BlockedBeforeEffect,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCleanupAssessment {
    NotReported,
    AllReportedComplete,
    Incomplete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamConsumptionAssessment {
    Completed,
    Cancelled,
    TimedOut,
    InvalidDeadline,
    DriverPanicked,
    SupervisorCancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamInitializationAssessment {
    NotObserved,
    Ready,
    PrefixRejected,
    TransitionFailed,
    Panicked,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamReportAssessment {
    pub frame: StreamFrameAssessment,
    pub prefix_durability: StreamPrefixDurability,
    pub cleanup: StreamCleanupAssessment,
    pub initialization: StreamInitializationAssessment,
    pub initialization_effect: Option<crate::execution_report::EffectBoundary>,
    pub consumption: Option<StreamConsumptionAssessment>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAssessmentError {
    Unavailable,
}

fn assess_prefix<T>(
    prefix: &crate::ExecutionReport<T>,
    extra: Option<&crate::SagaRollbackReport>,
) -> (StreamPrefixDurability, StreamCleanupAssessment) {
    use crate::ExecutionReport as Report;
    let durability = match prefix {
        Report::Unpersisted { .. } => StreamPrefixDurability::Unpersisted,
        Report::NotRequested { .. } => StreamPrefixDurability::NotRequested,
        Report::Recorded {
            completion: crate::execution_report::CompletionConfirmation::Confirmed,
            ..
        } => StreamPrefixDurability::RecordedCompletionConfirmed,
        Report::Recorded { .. } => StreamPrefixDurability::RecordedWithoutCompletion,
        Report::Unconfirmed { .. } => StreamPrefixDurability::Unconfirmed,
        Report::BlockedBeforeEffect { .. } => StreamPrefixDurability::BlockedBeforeEffect,
    };
    let mut cleanup = StreamCleanupAssessment::NotReported;
    for report in prefix.cleanup().into_iter().chain(extra) {
        if !report.is_complete() {
            return (durability, StreamCleanupAssessment::Incomplete);
        }
        cleanup = StreamCleanupAssessment::AllReportedComplete;
    }
    (durability, cleanup)
}
fn assess_joined<Out, E>(
    joined: &Result<Option<Original<Out, E>>, tokio::task::JoinError>,
) -> StreamReportAssessment {
    let mut assessment = StreamReportAssessment {
        frame: StreamFrameAssessment::FrameFailed,
        prefix_durability: StreamPrefixDurability::NotObserved,
        cleanup: StreamCleanupAssessment::NotReported,
        initialization: StreamInitializationAssessment::NotObserved,
        initialization_effect: None,
        consumption: None,
    };
    let report = match joined {
        Ok(None) => {
            assessment.frame = StreamFrameAssessment::RejectedBeforeStart;
            return assessment;
        }
        Ok(Some(Err(_))) => {
            assessment.frame = StreamFrameAssessment::AlreadyStarted;
            return assessment;
        }
        Err(_) => return assessment,
        Ok(Some(Ok(report))) => report,
    };
    assessment.frame = StreamFrameAssessment::Reported;
    let (durability, cleanup) = match report {
        StreamInitializationReport::Ready { prefix, value } => {
            assessment.initialization = StreamInitializationAssessment::Ready;
            assessment.initialization_effect =
                Some(crate::execution_report::EffectBoundary::MayHaveRun);
            assessment.consumption = Some(match &value.consumption {
                Ok(result) => match result.terminal {
                    StreamConsumptionTerminal::Completed => StreamConsumptionAssessment::Completed,
                    StreamConsumptionTerminal::Cancelled(_) => {
                        StreamConsumptionAssessment::Cancelled
                    }
                    StreamConsumptionTerminal::TimedOut(_) => StreamConsumptionAssessment::TimedOut,
                    StreamConsumptionTerminal::InvalidDeadline(_) => {
                        StreamConsumptionAssessment::InvalidDeadline
                    }
                },
                Err(crate::streaming_recovery::StreamDriverError::Panicked) => {
                    StreamConsumptionAssessment::DriverPanicked
                }
                Err(crate::streaming_recovery::StreamDriverError::Cancelled(_)) => {
                    StreamConsumptionAssessment::SupervisorCancelled
                }
            });
            assess_prefix(prefix, value.cleanup.as_ref())
        }
        StreamInitializationReport::Rejected { prefix } => {
            assessment.initialization = StreamInitializationAssessment::PrefixRejected;
            assessment.initialization_effect =
                Some(crate::execution_report::EffectBoundary::NotInvoked);
            assess_prefix(prefix, None)
        }
        StreamInitializationReport::Failed {
            prefix,
            error,
            initialization,
            initialization_cleanup,
            ..
        } => {
            assessment.initialization = match error {
                crate::streaming_initialization::StreamInitializationError::Transition(_) => {
                    StreamInitializationAssessment::TransitionFailed
                }
                crate::streaming_initialization::StreamInitializationError::Panicked => {
                    StreamInitializationAssessment::Panicked
                }
            };
            assessment.initialization_effect = Some(*initialization);
            assess_prefix(prefix, initialization_cleanup.as_ref())
        }
        StreamInitializationReport::Cancelled {
            prefix,
            initialization,
            initialization_cleanup,
            ..
        } => {
            assessment.initialization = StreamInitializationAssessment::Cancelled;
            assessment.initialization_effect = Some(*initialization);
            assess_prefix(prefix, initialization_cleanup.as_ref())
        }
    };
    assessment.prefix_durability = durability;
    assessment.cleanup = cleanup;
    assessment
}

/// Live state from the original typed owner, separate from copied report evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLiveRecoveryState {
    pub saga_stack_nonempty: bool,
    pub journal_present: bool,
    pub recovery_pending: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRecoveryProbeError {
    TimedOut,
    Unavailable,
    TraversalLimit,
    NotConfigured,
}
pub(crate) type RecoveryProbe = Arc<
    dyn Fn(
            Instant,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<StreamLiveRecoveryState, StreamRecoveryProbeError>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;
/// How a joined frame may be released by `release_settled_until`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamSettlement {
    /// Normal source end: prefix Saga inputs may be discarded.
    Completed,
    /// Other end with complete cleanup: no Saga input may remain.
    Ended,
    /// Cleanup was incomplete: only a fully reconciled journal may be released.
    RequiresReconciledJournal,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamLocalRetirementError {
    Wait(StreamReportWaitError),
    AssessmentUnavailable,
    RecoveryReviewRequired,
    NotConfigured,
    CustodyUnavailable,
    CustodyReleased,
    CustodyMismatch,
}
pub(crate) type LocalRetirement = Arc<
    dyn Fn(
            Instant,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), StreamLocalRetirementError>> + Send>,
        > + Send
        + Sync,
>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamLocalRecoveryError {
    Wait(StreamReportWaitError),
    FrameNotReported,
    Unavailable,
    JournalUnavailable,
    RecoveryBlocked,
    HandlerRegistryUnavailable,
}
pub(crate) type LocalRecovery = Arc<
    dyn Fn(
            Instant,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<crate::SagaRollbackOutcome, StreamLocalRecoveryError>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;
/// Observation returned by reading the existing external compensation key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamExternalReadback {
    /// The stored key exists and the claimed journal row was locally settled.
    /// The legacy key omits `fault_step`, so this does not establish a globally
    /// unique per-effect identity or exactly-once external execution.
    MarkerPresent,
    /// No marker exists for the current key. This does not prove the external
    /// action did not occur.
    MarkerAbsent,
    /// No marker store exists or the read failed/panicked; the row stays retained.
    ReadUnavailable,
}
/// Fixed failures while joining the original owner or claiming its journal row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamExternalRecoveryError {
    Wait(StreamReportWaitError),
    FrameNotReported,
    Unavailable,
    JournalUnavailable,
    RecoveryBlocked,
}
pub(crate) type ExternalRecovery = Arc<
    dyn Fn(
            Instant,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<StreamExternalReadback, StreamExternalRecoveryError>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

/// Clones share one original report. Waiting never retires recovery custody;
/// only explicit report transfer consumes the result, once across all clones.
/// Dropping the final lease is not successful cleanup or durable reconciliation.
pub struct StreamReportHandle<Out, E> {
    task: Arc<tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>>>,
    _owner: crate::nested_custody::Capability,
    retirement: Arc<AtomicU8>,
    assessment: Arc<Mutex<Option<StreamReportAssessment>>>,
    end_durability: Arc<Mutex<Option<crate::StreamDurability>>>,
    probe: Option<RecoveryProbe>,
    local_retirement: Option<LocalRetirement>,
    completed_finalization: Option<LocalRetirement>,
    local_recovery: Option<LocalRecovery>,
    external_recovery: Option<ExternalRecovery>,
}
impl<Out, E> Clone for StreamReportHandle<Out, E> {
    fn clone(&self) -> Self {
        Self {
            task: self.task.clone(),
            _owner: self._owner.clone(),
            retirement: self.retirement.clone(),
            assessment: self.assessment.clone(),
            end_durability: self.end_durability.clone(),
            probe: self.probe.clone(),
            local_retirement: self.local_retirement.clone(),
            completed_finalization: self.completed_finalization.clone(),
            local_recovery: self.local_recovery.clone(),
            external_recovery: self.external_recovery.clone(),
        }
    }
}
impl<Out: Send + 'static, E: Send + 'static> StreamReportHandle<Out, E> {
    pub(crate) fn new(
        task: Arc<
            tokio::sync::Mutex<crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>>,
        >,
        owner: crate::nested_custody::Capability,
    ) -> Self {
        Self {
            task,
            _owner: owner,
            retirement: Arc::new(AtomicU8::new(0)),
            assessment: Arc::new(Mutex::new(None)),
            end_durability: Arc::new(Mutex::new(None)),
            probe: None,
            local_retirement: None,
            completed_finalization: None,
            local_recovery: None,
            external_recovery: None,
        }
    }
    pub(crate) fn with_recovery_probe(mut self, probe: RecoveryProbe) -> Self {
        self.probe = Some(probe);
        self
    }
    pub(crate) fn with_local_retirement(mut self, retirement: LocalRetirement) -> Self {
        self.local_retirement = Some(retirement);
        self
    }
    pub(crate) fn with_completed_finalization(mut self, finalization: LocalRetirement) -> Self {
        self.completed_finalization = Some(finalization);
        self
    }
    pub(crate) fn with_local_recovery(mut self, recovery: LocalRecovery) -> Self {
        self.local_recovery = Some(recovery);
        self
    }
    pub(crate) fn with_external_recovery(mut self, recovery: ExternalRecovery) -> Self {
        self.external_recovery = Some(recovery);
        self
    }
    pub(crate) fn holder_capability(&self) -> crate::nested_custody::Capability {
        self.task.clone()
    }
    /// Explicitly retire local root custody after original Completed/Unpersisted
    /// execution with no retained Saga state or recovery journal. This does not
    /// confirm durable stream completion, external reversal or restart recovery.
    /// Clones share the exact decision, including after original report transfer.
    /// Other report/recovery/drain leases still retain recovery admission.
    pub async fn retire_completed_local_until(
        &self,
        deadline: Instant,
    ) -> Result<(), StreamLocalRetirementError> {
        (self
            .local_retirement
            .as_ref()
            .ok_or(StreamLocalRetirementError::NotConfigured)?)(deadline)
        .await
    }
    /// Release root custody once nothing in this stream needs reconciliation.
    ///
    /// Allowed when the frame was reported, no durability write is
    /// unconfirmed (prefix or stream end record; a recorded prefix needs an
    /// acknowledged end record) and one of these holds:
    /// - the source reached its normal end and no cleanup is incomplete: the
    ///   prefix Saga inputs are discarded, as an ordinary Axon discards them on
    ///   success;
    /// - the stream ended otherwise (prefix rejected, initialization failed or
    ///   cancelled, consumption cancelled or timed out) and its cleanup
    ///   completed, leaving no Saga input;
    /// - cleanup was incomplete, but operator recovery has since settled every
    ///   journal row and nothing is retained.
    ///
    /// Anything else returns `RecoveryReviewRequired` and keeps custody. This
    /// never runs compensation or replays work. Clones share the decision.
    pub async fn release_settled_until(
        &self,
        deadline: Instant,
    ) -> Result<(), StreamLocalRetirementError> {
        (self
            .completed_finalization
            .as_ref()
            .ok_or(StreamLocalRetirementError::NotConfigured)?)(deadline)
        .await
    }
    /// Durability of the stream end record, once the frame has been joined.
    /// `None` before the join, for a stream that never became ready, or when
    /// no journal was kept for this stream.
    pub fn stream_end_durability(
        &self,
    ) -> Result<Option<crate::StreamDurability>, StreamAssessmentError> {
        self.end_durability
            .lock()
            .map(|state| state.clone())
            .map_err(|_| StreamAssessmentError::Unavailable)
    }
    /// Explicitly run at most the first eligible local Saga compensation from
    /// this joined report's original typed owner. Results contain fixed outcome
    /// labels only; timeout or uncertain outcomes retain the original input.
    pub async fn reconcile_next_local_compensation_until(
        &self,
        deadline: Instant,
    ) -> Result<crate::SagaRollbackOutcome, StreamLocalRecoveryError> {
        let recovery = self
            .local_recovery
            .as_ref()
            .ok_or(StreamLocalRecoveryError::Unavailable)?;
        recovery(deadline).await
    }
    /// Read only the first retained external compensation marker from this
    /// report's original typed owner. This never calls the external hook. A
    /// missing marker or read failure keeps the original capability; a positive
    /// marker completes one row but never seals the journal. The result
    /// describes the existing key and does not establish a globally unique
    /// effect identity. The deadline bounds yielding store futures, but cannot
    /// preempt synchronous blocking code.
    pub async fn readback_next_external_compensation_until(
        &self,
        deadline: Instant,
    ) -> Result<StreamExternalReadback, StreamExternalRecoveryError> {
        let recovery = self
            .external_recovery
            .as_ref()
            .ok_or(StreamExternalRecoveryError::Unavailable)?;
        recovery(deadline).await
    }
    pub(crate) async fn require_reported_frame(
        &self,
        deadline: Instant,
    ) -> Result<(), StreamLocalRecoveryError> {
        if Instant::now() >= deadline {
            return Err(StreamLocalRecoveryError::Wait(
                StreamReportWaitError::TimedOut,
            ));
        }
        if self
            .assessment()
            .map_err(|_| StreamLocalRecoveryError::Unavailable)?
            .is_none()
        {
            match self.settle_until(deadline).await {
                Ok(()) | Err(StreamReportWaitError::AlreadyTaken) => {}
                Err(error) => return Err(StreamLocalRecoveryError::Wait(error)),
            }
        }
        let assessment = self
            .assessment()
            .map_err(|_| StreamLocalRecoveryError::Unavailable)?;
        match assessment {
            Some(assessment) if assessment.frame == StreamFrameAssessment::Reported => Ok(()),
            _ => Err(StreamLocalRecoveryError::FrameNotReported),
        }
    }
    pub(crate) async fn prove_completed_local(
        &self,
        deadline: Instant,
    ) -> Result<(), StreamLocalRetirementError> {
        use StreamLocalRetirementError as Error;
        if Instant::now() >= deadline {
            return Err(Error::Wait(StreamReportWaitError::TimedOut));
        }
        if self
            .assessment()
            .map_err(|_| Error::AssessmentUnavailable)?
            .is_none()
        {
            if let Err(error) = self.settle_until(deadline).await {
                if error != StreamReportWaitError::AlreadyTaken {
                    return Err(Error::Wait(error));
                }
            }
        }
        let assessment = self
            .assessment()
            .map_err(|_| Error::AssessmentUnavailable)?;
        match assessment {
            Some(StreamReportAssessment {
                frame: StreamFrameAssessment::Reported,
                prefix_durability: StreamPrefixDurability::Unpersisted,
                initialization: StreamInitializationAssessment::Ready,
                consumption: Some(StreamConsumptionAssessment::Completed),
                cleanup,
                ..
            }) if cleanup != StreamCleanupAssessment::Incomplete => Ok(()),
            _ => Err(Error::RecoveryReviewRequired),
        }
    }
    /// Classify a joined frame for [`Self::release_settled_until`]. The live
    /// owner state is checked separately under the context lock.
    pub(crate) async fn prove_settled(
        &self,
        deadline: Instant,
    ) -> Result<StreamSettlement, StreamLocalRetirementError> {
        use StreamLocalRetirementError as Error;
        if Instant::now() >= deadline {
            return Err(Error::Wait(StreamReportWaitError::TimedOut));
        }
        if self
            .assessment()
            .map_err(|_| Error::AssessmentUnavailable)?
            .is_none()
        {
            if let Err(error) = self.settle_until(deadline).await {
                if error != StreamReportWaitError::AlreadyTaken {
                    return Err(Error::Wait(error));
                }
            }
        }
        let assessment = self
            .assessment()
            .map_err(|_| Error::AssessmentUnavailable)?
            .ok_or(Error::RecoveryReviewRequired)?;
        let end = self
            .stream_end_durability()
            .map_err(|_| Error::AssessmentUnavailable)?;
        if assessment.frame != StreamFrameAssessment::Reported
            || matches!(end, Some(crate::StreamDurability::Unconfirmed { .. }))
        {
            return Err(Error::RecoveryReviewRequired);
        }
        let ready = assessment.initialization == StreamInitializationAssessment::Ready;
        match assessment.prefix_durability {
            StreamPrefixDurability::Unpersisted | StreamPrefixDurability::NotRequested => {}
            StreamPrefixDurability::RecordedCompletionConfirmed
            | StreamPrefixDurability::RecordedWithoutCompletion => {
                // A ready stream defers its completion to the end record.
                if ready && !matches!(end, Some(crate::StreamDurability::Recorded { .. })) {
                    return Err(Error::RecoveryReviewRequired);
                }
            }
            _ => return Err(Error::RecoveryReviewRequired),
        }
        Ok(
            if assessment.cleanup == StreamCleanupAssessment::Incomplete {
                StreamSettlement::RequiresReconciledJournal
            } else if ready
                && assessment.consumption == Some(StreamConsumptionAssessment::Completed)
            {
                StreamSettlement::Completed
            } else {
                StreamSettlement::Ended
            },
        )
    }
    /// Inspect the original owner with bounded context wait. This snapshot is
    /// neither retirement authorization nor proof of external reconciliation.
    pub async fn live_recovery_until(
        &self,
        deadline: Instant,
    ) -> Result<StreamLiveRecoveryState, StreamRecoveryProbeError> {
        let probe = self
            .probe
            .as_ref()
            .ok_or(StreamRecoveryProbeError::NotConfigured)?;
        (probe)(deadline).await
    }
    /// Join without taking the report. Timeout or dropped wait keeps the same
    /// frame/result available, including during mutex contention.
    pub async fn settle_until(&self, deadline: Instant) -> Result<(), StreamReportWaitError> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock())
            .await
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
    /// Remains available across clone/report transfer; None means not yet joined.
    /// This observation is not a caller-forgeable retirement authorization.
    pub fn assessment(&self) -> Result<Option<StreamReportAssessment>, StreamAssessmentError> {
        self.assessment
            .lock()
            .map(|state| *state)
            .map_err(|_| StreamAssessmentError::Unavailable)
    }
    fn observe_retirement(
        &self,
        task: &crate::managed_stream_task::ManagedStreamTask<Original<Out, E>>,
    ) {
        if let Some(joined) = task.joined_ref() {
            let rejected = matches!(joined, Ok(None));
            if let Ok(mut durability) = self.end_durability.lock() {
                *durability = match joined {
                    Ok(Some(Ok(StreamInitializationReport::Ready { value, .. }))) => {
                        value.end_durability.clone()
                    }
                    _ => None,
                };
            }
            if let Ok(mut assessment) = self.assessment.lock() {
                *assessment = Some(assess_joined(joined));
            }
            self.retirement
                .store(if rejected { 1 } else { 2 }, Ordering::Release);
        }
    }
    pub(crate) async fn prove_rejected_before_start(
        &self,
        deadline: Instant,
    ) -> Result<bool, StreamReportWaitError> {
        if self.retirement_evidence() == StreamRetirementEvidence::NotObserved {
            // A transferred report may already have populated the cache while
            // this waiter contended for the lock. Recheck after AlreadyTaken.
            if let Err(error) = self.settle_until(deadline).await {
                if error != StreamReportWaitError::AlreadyTaken
                    || self.retirement_evidence() == StreamRetirementEvidence::NotObserved
                {
                    return Err(error);
                }
            }
        }
        Ok(self.retirement_evidence() == StreamRetirementEvidence::RejectedBeforeStart)
    }
    /// Explicit single transfer of the original result; readiness never does this.
    pub async fn take_report_until(
        &self,
        deadline: Instant,
    ) -> Result<SupervisedStreamCompletion<Out, E>, StreamReportWaitError> {
        let mut task = tokio::time::timeout_at(deadline, self.task.lock())
            .await
            .map_err(|_| StreamReportWaitError::TimedOut)?;
        task.settle_until(deadline).await.map_err(wait_error)?;
        self.observe_retirement(&task);
        let joined = task
            .take_joined()
            .ok_or(StreamReportWaitError::AlreadyTaken)?;
        Ok(match joined {
            Err(error) => SupervisedStreamCompletion::FrameFailed(error),
            Ok(None) => SupervisedStreamCompletion::RejectedBeforeStart,
            Ok(Some(Err(_))) => SupervisedStreamCompletion::InvocationAlreadyStarted,
            Ok(Some(Ok(report))) => SupervisedStreamCompletion::Report(report.map_value(|ready| {
                SupervisedStreamReady {
                    consumption: ready.consumption.map(|consumed| consumed.terminal).map_err(
                        |error| match error {
                            crate::streaming_recovery::StreamDriverError::Panicked => {
                                SupervisedStreamFailure::DriverPanicked
                            }
                            crate::streaming_recovery::StreamDriverError::Cancelled(context) => {
                                SupervisedStreamFailure::Cancelled(context)
                            }
                        },
                    ),
                    cleanup: ready.cleanup,
                }
            })),
        })
    }
}
fn wait_error(error: crate::managed_stream_task::ManagedTaskWaitError) -> StreamReportWaitError {
    match error {
        crate::managed_stream_task::ManagedTaskWaitError::TimedOut => {
            StreamReportWaitError::TimedOut
        }
        _ => StreamReportWaitError::AlreadyTaken,
    }
}

#[cfg(test)]
mod assessment_tests {
    use super::*;
    async fn joined_original_fixture(
        original: Original<u32, String>,
    ) -> (
        crate::nested_custody::CustodyStore,
        StreamReportHandle<u32, String>,
    ) {
        use crate::streaming_recovery::{RecoveryCustody, SupervisorObservation, SupervisorPhase};
        let root = crate::nested_custody::CustodyStore::new(1);
        let owner: crate::nested_custody::Capability = Arc::new(());
        let mut frame =
            crate::managed_stream_task::PreparedStreamTask::new_owned(owner.clone(), async move {
                original
            });
        let mut slot = root.reserve_stream_slot().unwrap();
        let journal = crate::SagaRollbackJournal::new("assessment fixture");
        let (_, observer) = tokio::sync::watch::channel(SupervisorObservation {
            phase: SupervisorPhase::NotStarted,
            disposition: None,
            cleanup_complete: None,
            cleanup_timed_out: false,
            custody: RecoveryCustody::Held,
        });
        let receipt = slot
            .finalize_managed(&root.client(), owner.clone(), &frame, &journal, observer)
            .unwrap();
        let handle = StreamReportHandle::new(frame.holder(), owner);
        frame.grant_registered_start(&receipt).unwrap();
        handle
            .take_report_until(Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        (root, handle)
    }
    #[tokio::test]
    async fn completed_local_proof_uses_original_durability_cleanup_and_terminal_after_transfer() {
        use crate::ExecutionReport as Report;
        use crate::execution_report::{
            CompletionConfirmation, EffectBoundary, PersistenceOperation,
        };
        for cleanup_state in [None, Some(true), Some(false)] {
            let prefixes = [
                Report::Unpersisted {
                    result: (),
                    cleanup: None,
                },
                Report::NotRequested {
                    result: (),
                    cleanup: None,
                },
                Report::Recorded {
                    result: (),
                    completion: CompletionConfirmation::Confirmed,
                    cleanup: None,
                },
                Report::Recorded {
                    result: (),
                    completion: CompletionConfirmation::NotRequested,
                    cleanup: None,
                },
                Report::Unconfirmed {
                    result: (),
                    operation: PersistenceOperation::Complete,
                    effect: EffectBoundary::MayHaveRun,
                    cleanup: None,
                },
                Report::BlockedBeforeEffect {
                    operation: PersistenceOperation::Intent,
                    effect: EffectBoundary::NotInvoked,
                    cleanup: None,
                },
            ];
            for (index, prefix) in prefixes.into_iter().enumerate() {
                let original = Ok(StreamInitializationReport::Ready {
                    prefix: prefix.with_cleanup(cleanup_state.map(cleanup)),
                    value: crate::streaming_recovery::StreamReadyResult {
                        consumption: Ok(crate::streaming_recovery::StreamDriveResult {
                            value: (),
                            terminal: StreamConsumptionTerminal::Completed,
                        }),
                        cleanup: None,
                        end_durability: None,
                    },
                });
                let (_root, handle) = joined_original_fixture(original).await;
                assert_eq!(
                    handle
                        .prove_completed_local(Instant::now() + std::time::Duration::from_secs(5))
                        .await,
                    if index == 0 && cleanup_state != Some(false) {
                        Ok(())
                    } else {
                        Err(StreamLocalRetirementError::RecoveryReviewRequired)
                    }
                );
            }
        }
        for consumption in [
            Err(crate::streaming_recovery::StreamDriverError::Panicked),
            Ok(crate::streaming_recovery::StreamDriveResult {
                value: (),
                terminal: StreamConsumptionTerminal::Cancelled(
                    ranvier_core::CancellationContext::new(
                        ranvier_core::CancellationReason::Explicit,
                    ),
                ),
            }),
        ] {
            let (_root, handle) = joined_original_fixture(Ok(StreamInitializationReport::Ready {
                prefix: Report::Unpersisted {
                    result: (),
                    cleanup: None,
                },
                value: crate::streaming_recovery::StreamReadyResult {
                    consumption,
                    cleanup: Some(cleanup(true)),
                    end_durability: None,
                },
            }))
            .await;
            assert_eq!(
                handle
                    .prove_completed_local(Instant::now() + std::time::Duration::from_secs(5))
                    .await,
                Err(StreamLocalRetirementError::RecoveryReviewRequired)
            );
        }
    }
    fn cleanup(finished: bool) -> crate::SagaRollbackReport {
        crate::SagaRollbackReport {
            trace_id: "private cleanup trace".into(),
            finished,
            steps: Vec::new(),
        }
    }
    #[test]
    fn assessment_preserves_unknown_durability_and_incomplete_cleanup_without_private_text() {
        let original: Original<u32, String> = Ok(StreamInitializationReport::Failed {
            prefix: crate::ExecutionReport::Unconfirmed {
                result: (),
                operation: crate::execution_report::PersistenceOperation::Complete,
                effect: crate::execution_report::EffectBoundary::MayHaveRun,
                cleanup: Some(cleanup(false)),
            },
            error: crate::streaming_initialization::StreamInitializationError::Transition(
                "private initializer secret".into(),
            ),
            initialization: crate::execution_report::EffectBoundary::NotInvoked,
            initialization_cleanup: Some(cleanup(true)),
        });
        let assessed = assess_joined(&Ok(Some(original)));
        assert_eq!(
            assessed.prefix_durability,
            StreamPrefixDurability::Unconfirmed
        );
        assert_eq!(assessed.cleanup, StreamCleanupAssessment::Incomplete);
        assert_eq!(
            assessed.initialization,
            StreamInitializationAssessment::TransitionFailed
        );
        assert_eq!(
            assessed.initialization_effect,
            Some(crate::execution_report::EffectBoundary::NotInvoked)
        );
        assert!(!format!("{assessed:?}").contains("private"));
    }
    #[test]
    fn prefix_assessment_distinguishes_every_persistence_contract() {
        use crate::ExecutionReport as Report;
        let reports = [
            (
                Report::Unpersisted {
                    result: (),
                    cleanup: None,
                },
                StreamPrefixDurability::Unpersisted,
            ),
            (
                Report::NotRequested {
                    result: (),
                    cleanup: None,
                },
                StreamPrefixDurability::NotRequested,
            ),
            (
                Report::Recorded {
                    result: (),
                    completion: crate::execution_report::CompletionConfirmation::Confirmed,
                    cleanup: None,
                },
                StreamPrefixDurability::RecordedCompletionConfirmed,
            ),
            (
                Report::Recorded {
                    result: (),
                    completion: crate::execution_report::CompletionConfirmation::NotRequested,
                    cleanup: None,
                },
                StreamPrefixDurability::RecordedWithoutCompletion,
            ),
            (
                Report::Unconfirmed {
                    result: (),
                    operation: crate::execution_report::PersistenceOperation::Complete,
                    effect: crate::execution_report::EffectBoundary::MayHaveRun,
                    cleanup: None,
                },
                StreamPrefixDurability::Unconfirmed,
            ),
            (
                Report::BlockedBeforeEffect {
                    operation: crate::execution_report::PersistenceOperation::Intent,
                    effect: crate::execution_report::EffectBoundary::NotInvoked,
                    cleanup: None,
                },
                StreamPrefixDurability::BlockedBeforeEffect,
            ),
        ];
        for (report, expected) in reports {
            assert_eq!(
                assess_prefix(&report, None),
                (expected, StreamCleanupAssessment::NotReported)
            );
            assert_eq!(
                assess_prefix(&report, Some(&cleanup(true))),
                (expected, StreamCleanupAssessment::AllReportedComplete)
            );
            assert_eq!(
                assess_prefix(&report, Some(&cleanup(false))),
                (expected, StreamCleanupAssessment::Incomplete)
            );
        }
    }
    #[tokio::test]
    async fn poisoned_assessment_fails_closed_without_losing_original_join_error() {
        let prepared = crate::managed_stream_task::PreparedStreamTask::new(async {
            Err::<
                StreamInitializationReport<
                    u32,
                    crate::streaming_recovery::StreamReadyResult<()>,
                    String,
                >,
                _,
            >(crate::streaming_recovery::StreamInvocationAlreadyStarted)
        });
        prepared.holder().lock().await.abort_frame();
        let handle = StreamReportHandle::new(prepared.holder(), Arc::new(()));
        let cache = handle.assessment.clone();
        assert!(
            std::thread::spawn(move || {
                let _lock = cache.lock().unwrap();
                panic!("cache poison fixture");
            })
            .join()
            .is_err()
        );
        let report = handle
            .take_report_until(Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(report, SupervisedStreamCompletion::FrameFailed(error) if error.is_cancelled())
        );
        assert_eq!(handle.assessment(), Err(StreamAssessmentError::Unavailable));
        assert_eq!(
            handle
                .prove_completed_local(Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Err(StreamLocalRetirementError::AssessmentUnavailable)
        );
        assert_eq!(
            handle.retirement_evidence(),
            StreamRetirementEvidence::RequiresRecoveryReview
        );
    }
    #[tokio::test]
    async fn assessment_survives_original_report_transfer_and_clone_without_authorizing_retirement()
    {
        let original: Original<u32, String> = Ok(StreamInitializationReport::Ready {
            prefix: crate::ExecutionReport::Recorded {
                result: (),
                completion: crate::execution_report::CompletionConfirmation::NotRequested,
                cleanup: Some(cleanup(true)),
            },
            value: crate::streaming_recovery::StreamReadyResult {
                consumption: Ok(crate::streaming_recovery::StreamDriveResult {
                    value: (),
                    terminal: StreamConsumptionTerminal::Completed,
                }),
                cleanup: None,
                end_durability: None,
            },
        });
        let prepared = crate::managed_stream_task::PreparedStreamTask::new(async move { original });
        let holder = prepared.holder();
        let handle = StreamReportHandle::new(holder, Arc::new(()));
        let clone = handle.clone();
        assert_eq!(clone.assessment().unwrap(), None);
        // This fixture intentionally rejects the gate; original report must not
        // be substituted for the actual joined no-start result.
        drop(prepared);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        assert!(matches!(
            handle.take_report_until(deadline).await.unwrap(),
            SupervisedStreamCompletion::RejectedBeforeStart
        ));
        let assessment = clone.assessment().unwrap().unwrap();
        assert_eq!(assessment.frame, StreamFrameAssessment::RejectedBeforeStart);
        assert_eq!(
            assessment.prefix_durability,
            StreamPrefixDurability::NotObserved
        );
        assert_eq!(assessment.cleanup, StreamCleanupAssessment::NotReported);
        assert_eq!(clone.assessment().unwrap(), Some(assessment));
    }
}

type RecoveryFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Recovery operations of a report handle that do not depend on its typed
/// output or error.
trait StreamRecoveryOps: Send + Sync {
    fn assessment(&self) -> Result<Option<StreamReportAssessment>, StreamAssessmentError>;
    fn stream_end_durability(
        &self,
    ) -> Result<Option<crate::StreamDurability>, StreamAssessmentError>;
    fn reconcile(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<crate::SagaRollbackOutcome, StreamLocalRecoveryError>>;
    fn readback(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<StreamExternalReadback, StreamExternalRecoveryError>>;
    fn release(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<(), StreamLocalRetirementError>>;
    fn live(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<StreamLiveRecoveryState, StreamRecoveryProbeError>>;
}
impl<Out: Send + 'static, E: Send + 'static> StreamRecoveryOps for StreamReportHandle<Out, E> {
    fn assessment(&self) -> Result<Option<StreamReportAssessment>, StreamAssessmentError> {
        StreamReportHandle::assessment(self)
    }
    fn stream_end_durability(
        &self,
    ) -> Result<Option<crate::StreamDurability>, StreamAssessmentError> {
        StreamReportHandle::stream_end_durability(self)
    }
    fn reconcile(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<crate::SagaRollbackOutcome, StreamLocalRecoveryError>> {
        Box::pin(self.reconcile_next_local_compensation_until(deadline))
    }
    fn readback(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<StreamExternalReadback, StreamExternalRecoveryError>> {
        Box::pin(self.readback_next_external_compensation_until(deadline))
    }
    fn release(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<(), StreamLocalRetirementError>> {
        Box::pin(self.release_settled_until(deadline))
    }
    fn live(
        &self,
        deadline: Instant,
    ) -> RecoveryFuture<'_, Result<StreamLiveRecoveryState, StreamRecoveryProbeError>> {
        Box::pin(self.live_recovery_until(deadline))
    }
}

/// A supervised stream's recovery operations without its typed report.
///
/// Adapters hand this to an operator when a stream is held for recovery
/// review. It keeps the stream's custody alive while held. Each method has the
/// same meaning as on [`StreamReportHandle`]; nothing here replays work, and
/// in-memory custody ends with the process (restart recovery uses the trace
/// store).
#[derive(Clone)]
pub struct StreamRecoveryHandle {
    inner: Arc<dyn StreamRecoveryOps>,
}
impl StreamRecoveryHandle {
    /// See [`StreamReportHandle::assessment`].
    pub fn assessment(&self) -> Result<Option<StreamReportAssessment>, StreamAssessmentError> {
        self.inner.assessment()
    }
    /// See [`StreamReportHandle::stream_end_durability`].
    pub fn stream_end_durability(
        &self,
    ) -> Result<Option<crate::StreamDurability>, StreamAssessmentError> {
        self.inner.stream_end_durability()
    }
    /// See [`StreamReportHandle::reconcile_next_local_compensation_until`].
    pub async fn reconcile_next_local_compensation_until(
        &self,
        deadline: Instant,
    ) -> Result<crate::SagaRollbackOutcome, StreamLocalRecoveryError> {
        self.inner.reconcile(deadline).await
    }
    /// See [`StreamReportHandle::readback_next_external_compensation_until`].
    pub async fn readback_next_external_compensation_until(
        &self,
        deadline: Instant,
    ) -> Result<StreamExternalReadback, StreamExternalRecoveryError> {
        self.inner.readback(deadline).await
    }
    /// See [`StreamReportHandle::release_settled_until`].
    pub async fn release_settled_until(
        &self,
        deadline: Instant,
    ) -> Result<(), StreamLocalRetirementError> {
        self.inner.release(deadline).await
    }
    /// See [`StreamReportHandle::live_recovery_until`].
    pub async fn live_recovery_until(
        &self,
        deadline: Instant,
    ) -> Result<StreamLiveRecoveryState, StreamRecoveryProbeError> {
        self.inner.live(deadline).await
    }
}
impl<Out: Send + 'static, E: Send + 'static> StreamReportHandle<Out, E> {
    /// A recovery handle that does not carry the typed report.
    pub fn recovery_handle(&self) -> StreamRecoveryHandle {
        StreamRecoveryHandle {
            inner: Arc::new(self.clone()),
        }
    }
}
