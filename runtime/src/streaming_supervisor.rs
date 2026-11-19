//! Paired in-process reservations before allocating an owned stream context.
//! Capacity counts owners, not bytes. Reservations alone do not prove durability.
use crate::nested_custody::{CustodyReservation, CustodyStore, WeakCustodyClient};
use crate::streaming_axon::OwnedStreamContext;
use crate::streaming_recovery_admission::{RecoveryPermit, StreamRecoveryAdmission};

/// Root owner for finite stream admission and live recovery records.
/// Keep this owner alive through execution and explicit drain/retirement.
/// Dropping it does not run asynchronous cleanup or reconcile external effects.
pub struct StreamSupervisor {
    pub(crate) admission: StreamRecoveryAdmission,
    pub(crate) root: CustodyStore,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReservationError {
    RecoveryCapacity,
    RecoveryClosed,
    RecoveryUnavailable,
    RootCapacity,
    RootClosed,
    RootUnavailable,
}
/// Admission failure has no factory result; factory rejection retains its exact X.
pub enum StreamContextFactoryError<X> {
    Admission(StreamReservationError),
    Factory(X),
    AlreadyCreated,
}
impl<X> std::fmt::Debug for StreamContextFactoryError<X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(reason) => f.debug_tuple("Admission").field(reason).finish(),
            Self::Factory(_) => f.write_str("StreamContextFactoryError::Factory"),
            Self::AlreadyCreated => f.write_str("StreamContextFactoryError::AlreadyCreated"),
        }
    }
}
fn root_error(signal: &'static str) -> StreamReservationError {
    match signal {
        "execution.nested_custody_capacity" => StreamReservationError::RootCapacity,
        "execution.nested_custody_admission_closed" | "execution.parent_custody_released" => {
            StreamReservationError::RootClosed
        }
        _ => StreamReservationError::RootUnavailable,
    }
}
fn recovery_error(
    error: crate::streaming_recovery_admission::RecoveryAdmissionFailure,
) -> StreamReservationError {
    use crate::streaming_recovery_admission::RecoveryAdmissionFailure as Failure;
    match error {
        Failure::Capacity => StreamReservationError::RecoveryCapacity,
        Failure::Closed => StreamReservationError::RecoveryClosed,
        _ => StreamReservationError::RecoveryUnavailable,
    }
}
/// Unique reservation of one recovery owner and one root record.
/// Drop refunds undecided slots. This is neither cloneable nor serializable.
pub struct StreamReservation {
    pub(crate) admission: StreamRecoveryAdmission,
    pub(crate) permit: RecoveryPermit,
    pub(crate) client: WeakCustodyClient,
    pub(crate) slot: CustodyReservation,
    context_created: bool,
}
impl StreamSupervisor {
    pub fn new(capacity: usize) -> Self {
        Self {
            admission: StreamRecoveryAdmission::new(capacity),
            root: CustodyStore::new(capacity),
        }
    }
    /// Reserve both finite capacities before invoking request/context callbacks.
    /// Partial failure refunds the first reservation before returning.
    pub fn reserve(&self) -> Result<StreamReservation, StreamReservationError> {
        let permit = self.admission.reserve().map_err(recovery_error)?;
        let slot = self.root.reserve_stream_slot().map_err(root_error)?;
        Ok(StreamReservation {
            admission: self.admission.clone(),
            permit,
            client: self.root.client(),
            slot,
            context_created: false,
        })
    }
    /// Stop both new admissions. Existing records remain retained for draining.
    /// Close attempts both authorities even when one is unavailable.
    pub fn close(&self) -> Result<(), StreamReservationError> {
        let recovery = self
            .admission
            .close()
            .map_err(|_| StreamReservationError::RecoveryUnavailable);
        let root = self
            .root
            .close_admission()
            .map_err(|_| StreamReservationError::RootUnavailable);
        recovery.and(root)
    }
}
/// Heterogeneous task/owner lease. Settlement retains original typed reports.
/// Neither settlement nor dropping this lease authorizes recovery retirement.
pub struct StreamDrainLease {
    family: crate::nested_custody::ErasedTaskFamily,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDrainStatus {
    Pending,
    Settled,
    TimedOut,
    ReportAlreadyTransferred,
    Unavailable,
}
impl StreamSupervisor {
    /// Close admissions and retain every registered managed task before waiting.
    /// An unfinalized reservation cannot attach a task after this snapshot.
    pub fn close_for_drain(&self) -> Result<StreamDrainLease, &'static str> {
        let recovery = self.admission.close();
        let family = self.root.close_and_erased_family()?;
        recovery.map_err(|_| "execution.stream_recovery_admission_unavailable")?;
        Ok(StreamDrainLease { family })
    }
}
impl StreamDrainLease {
    pub fn unfinalized_reservations(&self) -> usize {
        self.family.reserved
    }
    pub async fn settle_until(&mut self, deadline: tokio::time::Instant) {
        self.family.settle_until(deadline).await;
    }
    /// IDs are in-process correlation only, never persisted execution identity.
    pub fn statuses(&self) -> Vec<(uuid::Uuid, StreamDrainStatus)> {
        use crate::managed_stream_task::ManagedTaskWaitError as Error;
        self.family
            .task_ids()
            .into_iter()
            .map(|id| {
                let status = self
                    .family
                    .statuses
                    .iter()
                    .find(|(candidate, _)| *candidate == id);
                (
                    id,
                    match status.map(|(_, status)| status) {
                        None => StreamDrainStatus::Pending,
                        Some(Ok(())) => StreamDrainStatus::Settled,
                        Some(Err(Error::TimedOut)) => StreamDrainStatus::TimedOut,
                        Some(Err(Error::AlreadyJoined)) => {
                            StreamDrainStatus::ReportAlreadyTransferred
                        }
                        _ => StreamDrainStatus::Unavailable,
                    },
                )
            })
            .collect()
    }
}
impl StreamReservation {
    /// Invoke one context factory only after paired admission succeeded.
    /// A successful ticket cannot be reused for another context factory.
    /// Factory errors or panic unwind refund both reservations. Successful return
    /// retains the unique ticket alongside the exact context for later binding.
    /// Authorities are rechecked before the callback, without holding locks over
    /// user code. A concurrent close after that check may let an admitted callback
    /// finish; later frame binding still rejects closed root admission.
    pub fn with_context<Res, X>(
        mut self,
        factory: impl FnOnce() -> Result<OwnedStreamContext<Res>, X>,
    ) -> Result<(Self, OwnedStreamContext<Res>), StreamContextFactoryError<X>> {
        if self.context_created {
            return Err(StreamContextFactoryError::AlreadyCreated);
        }
        self.context_created = true;
        self.admission
            .validate_open_permit(&self.permit)
            .map_err(|error| StreamContextFactoryError::Admission(recovery_error(error)))?;
        self.slot
            .validate_open(&self.client)
            .map_err(|signal| StreamContextFactoryError::Admission(root_error(signal)))?;
        let context = factory().map_err(StreamContextFactoryError::Factory)?;
        Ok((self, context))
    }
}

/// Setup rejection preserves the original request context and input. Later
/// preparation rejection retains an opaque recovery lease instead of discarding it.
pub enum StreamPrepareError<In, Out, E, Res> {
    Staging(StreamStartRejected<Out, E>),
    Setup {
        signal: &'static str,
        input: In,
        context: OwnedStreamContext<Res>,
    },
    Preparation {
        signal: &'static str,
        input: In,
        recovery: StreamRecoveryLease,
    },
}
/// In-process owner lease only; Drop does not reconcile external effects.
pub struct StreamRecoveryLease {
    _owner: crate::nested_custody::Capability,
}
type PreparedFrame<Out, E> = crate::managed_stream_task::PreparedStreamTask<
    Result<
        crate::StreamInitializationReport<Out, crate::streaming_recovery::StreamReadyResult<()>, E>,
        crate::streaming_recovery::StreamInvocationAlreadyStarted,
    >,
>;
/// Prepared exact frame, with no public raw start permit.
pub struct PreparedSupervisedStream<In, Out, Item, E, Res> {
    plan: crate::streaming_axon::OwnedCallablePlan<In, Out, Item, E, Res>,
    prepared: PreparedFrame<Out, E>,
    custody: crate::nested_custody::WeakCustody,
    journal: crate::SagaRollbackJournal,
    report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    client: WeakCustodyClient,
    items: tokio::sync::mpsc::Receiver<Item>,
    readiness: crate::streaming_axon::StreamReadiness,
}
/// Actual invocation, item transport and original typed report have distinct roles.
/// Keep the supervisor alive; dropping this value requests cancellation, while
/// the root and report leases retain recovery ownership for later settlement.
pub struct SupervisedStreamInvocation<Out, Item, E> {
    pub items: tokio::sync::mpsc::Receiver<Item>,
    pub readiness: crate::streaming_axon::StreamReadiness,
    report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    control: ranvier_core::CancellationToken,
    _custody: crate::nested_custody::WeakCustody,
    _journal: crate::SagaRollbackJournal,
}
impl<Out, Item, E> SupervisedStreamInvocation<Out, Item, E> {
    /// Read/clone the original report lease; callers cannot replace its bound assessment.
    pub fn report(&self) -> &crate::streaming_report_handle::StreamReportHandle<Out, E> {
        &self.report
    }
    pub fn cancel(&self, reason: ranvier_core::CancellationReason) {
        self.control.cancel(reason);
    }
}
impl<Out, Item, E> Drop for SupervisedStreamInvocation<Out, Item, E> {
    fn drop(&mut self) {
        self.control
            .cancel(ranvier_core::CancellationReason::ClientDisconnected);
    }
}
/// Rejected start still exposes bounded access to the exact rejected/failed frame.
pub struct StreamStartRejected<Out, E> {
    pub signal: &'static str,
    report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    _custody: Option<crate::nested_custody::WeakCustody>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRetirementError {
    Wait(crate::streaming_report_handle::StreamReportWaitError),
    RecoveryReviewRequired,
}
impl<Out: Send + 'static, E: Send + 'static> StreamStartRejected<Out, E> {
    /// Read or clone the report lease without replacing the exact retirement proof.
    pub fn report(&self) -> &crate::streaming_report_handle::StreamReportHandle<Out, E> {
        &self.report
    }
    /// Release a finalized local root record only after joining the exact frame
    /// and proving its start gate was rejected. Repeated calls are idempotent.
    /// Executed, panicked or unknown frames cannot use this no-effect authority.
    pub async fn retire_unstarted_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), StreamRetirementError> {
        if !self
            .report
            .prove_rejected_before_start(deadline)
            .await
            .map_err(StreamRetirementError::Wait)?
        {
            return Err(StreamRetirementError::RecoveryReviewRequired);
        }
        if let Some(custody) = &self._custody {
            custody.release();
        }
        Ok(())
    }
}
impl StreamReservation {
    pub fn prepare<In, Out, Item, E, Res>(
        self,
        circuit: &crate::streaming_axon::StreamingAxon<In, Out, Item, E, Res>,
        context: OwnedStreamContext<Res>,
        input: In,
        cleanup_grace: std::time::Duration,
        expires: Option<std::time::Instant>,
    ) -> Result<PreparedSupervisedStream<In, Out, Item, E, Res>, StreamPrepareError<In, Out, E, Res>>
    where
        In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
        E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
        Item: Send + 'static,
        Res: Send + Sync + 'static,
    {
        if circuit.owned_definition.buffer_size == 0 {
            return Err(StreamPrepareError::Setup {
                signal: "execution.stream_buffer_invalid",
                context,
                input,
            });
        }
        if let Err(signal) = self.slot.validate_open(&self.client) {
            return Err(StreamPrepareError::Setup {
                signal,
                context,
                input,
            });
        }
        let Self {
            admission,
            permit,
            client,
            mut slot,
            ..
        } = self;
        let mut plan = match circuit
            .owned_definition
            .try_callable_plan_reserved(context, &admission, expires, permit)
        {
            Ok(plan) => plan,
            Err(error) => {
                let (signal, context) = match error {
                    crate::streaming_axon::OwnedStreamPlanRejected::InitializerType { context } => {
                        ("execution.stream_initializer_mismatch", context)
                    }
                    crate::streaming_axon::OwnedStreamPlanRejected::Admission(error) => {
                        let (_, context) = error.into_parts();
                        ("execution.stream_recovery_admission_rejected", context)
                    }
                };
                return Err(StreamPrepareError::Setup {
                    signal,
                    input,
                    context,
                });
            }
        };
        let ((mut prepared, items), readiness) =
            match plan.prepare_bounded_ready(input, cleanup_grace) {
                Ok(frame) => frame,
                Err(error) => {
                    return Err(StreamPrepareError::Preparation {
                        signal: error.signal,
                        input: error.input,
                        recovery: StreamRecoveryLease {
                            _owner: plan.owner.retention_capability(),
                        },
                    });
                }
            };
        let journal = crate::SagaRollbackJournal::new("supervised-stream-root");
        let report = crate::streaming_report_handle::StreamReportHandle::new(
            prepared.holder(),
            std::sync::Arc::new((plan.owner.retention_capability(), journal.clone())),
        )
        .with_recovery_probe(plan.owner.recovery_probe());
        let custody = match plan
            .owner
            .stage_reserved(&client, &mut slot, &journal, &mut prepared)
        {
            Ok(custody) => custody,
            Err(error) => {
                return Err(StreamPrepareError::Staging(StreamStartRejected {
                    signal: error.signal,
                    report,
                    _custody: error.custody,
                }));
            }
        };
        let retirement = plan.owner.local_retirement(custody.clone(), report.clone());
        let recovery = plan.owner.local_recovery(report.clone());
        let external_recovery = plan.owner.external_recovery(report.clone());
        let report = report
            .with_local_retirement(retirement)
            .with_local_recovery(recovery)
            .with_external_recovery(external_recovery);
        Ok(PreparedSupervisedStream {
            plan,
            prepared,
            custody,
            journal,
            report,
            client,
            items,
            readiness,
        })
    }
}
impl<In, Out, Item, E, Res> PreparedSupervisedStream<In, Out, Item, E, Res>
where
    In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
    Res: Send + Sync + 'static,
{
    /// Clone this lease before abandoning preparation if the caller needs the
    /// original rejection report; the root also retains the exact staged frame.
    pub fn report(&self) -> &crate::streaming_report_handle::StreamReportHandle<Out, E> {
        &self.report
    }
    pub fn abandon(mut self) -> StreamStartRejected<Out, E> {
        let _ = self.prepared.reject_start();
        StreamStartRejected {
            signal: "execution.stream_preparation_abandoned",
            report: self.report,
            _custody: Some(self.custody),
        }
    }
    pub async fn start(
        mut self,
        parent: ranvier_core::CancellationToken,
    ) -> Result<SupervisedStreamInvocation<Out, Item, E>, StreamStartRejected<Out, E>> {
        let control = parent.child_token();
        if let Err(signal) = self.plan.owner.start_staged(
            self.client,
            &self.custody,
            control.clone(),
            &mut self.prepared,
        ) {
            return Err(StreamStartRejected {
                signal,
                report: self.report,
                _custody: Some(self.custody),
            });
        }
        Ok(SupervisedStreamInvocation {
            items: self.items,
            readiness: self.readiness,
            report: self.report,
            control,
            _custody: self.custody,
            _journal: self.journal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[derive(Clone)]
    struct Finite;
    #[async_trait::async_trait]
    impl ranvier_core::streaming::StreamingTransition<u32> for Finite {
        type Item = u32;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: u32,
            _: &(),
            _: &mut ranvier_core::Bus,
        ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String>
        {
            Ok(Box::pin(futures_util::stream::iter([input])))
        }
    }
    #[derive(Clone)]
    struct Prefix;
    #[async_trait::async_trait]
    impl ranvier_core::Transition<u32, u32> for Prefix {
        type Error = String;
        type Resources = ();
        async fn run(
            &self,
            input: u32,
            _: &(),
            _: &mut ranvier_core::Bus,
        ) -> ranvier_core::Outcome<u32, String> {
            ranvier_core::Outcome::Next(input)
        }
    }
    #[derive(Clone)]
    struct CountUndo(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl ranvier_core::Transition<u32, ()> for CountUndo {
        type Error = String;
        type Resources = ();
        async fn run(
            &self,
            input: u32,
            _: &(),
            bus: &mut ranvier_core::Bus,
        ) -> ranvier_core::Outcome<(), String> {
            assert!(bus.cancellation_token().is_some());
            assert_eq!(input, 251);
            self.0.fetch_add(1, Ordering::SeqCst);
            ranvier_core::Outcome::Next(())
        }
    }
    #[derive(Clone)]
    struct CaptureExternalJournal {
        trace_id: String,
        captured: Arc<std::sync::Mutex<Option<crate::SagaRollbackJournal>>>,
        prior_local: bool,
    }
    #[async_trait::async_trait]
    impl ranvier_core::Transition<u32, u32> for CaptureExternalJournal {
        type Error = String;
        type Resources = ();
        async fn run(
            &self,
            input: u32,
            _: &(),
            bus: &mut ranvier_core::Bus,
        ) -> ranvier_core::Outcome<u32, String> {
            bus.insert(crate::PersistenceTraceId::new(self.trace_id.clone()));
            let journal = crate::SagaRollbackJournal::new(&self.trace_id);
            if self.prior_local {
                journal.push_uninvoked_task(ranvier_core::saga::SagaTask {
                    node_id: "earlier-local-step".into(),
                    node_label: "private local label".into(),
                    input_snapshot: vec![1, 2, 3],
                });
            }
            bus.insert(journal.clone());
            *self.captured.lock().unwrap() = Some(journal);
            ranvier_core::Outcome::Next(input)
        }
    }
    #[derive(Clone)]
    struct ExternalFailure;
    #[async_trait::async_trait]
    impl ranvier_core::Transition<u32, u32> for ExternalFailure {
        type Error = String;
        type Resources = ();
        async fn run(
            &self,
            _: u32,
            _: &(),
            _: &mut ranvier_core::Bus,
        ) -> ranvier_core::Outcome<u32, String> {
            ranvier_core::Outcome::Fault("original external compensation fixture fault".into())
        }
    }
    #[derive(Clone, Copy)]
    enum ExternalReadBehavior {
        Normal,
        FailSecond,
        PendingSecond,
        DropPanicSecond,
        PollPanicSecond,
        BlockingPastDeadlineSecond,
    }
    struct PanicOnDropPending;
    impl std::future::Future for PanicOnDropPending {
        type Output = anyhow::Result<bool>;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }
    impl Drop for PanicOnDropPending {
        fn drop(&mut self) {
            panic!("private store future drop panic");
        }
    }
    struct ExternalRecoveryHook {
        calls: Arc<AtomicUsize>,
        succeed: bool,
    }
    #[async_trait::async_trait]
    impl crate::CompensationHook for ExternalRecoveryHook {
        async fn compensate(&self, _: crate::CompensationContext) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.succeed {
                Ok(())
            } else {
                Err(anyhow::anyhow!("private hook failure"))
            }
        }
    }
    struct ExternalRecoveryMarker {
        applied: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
        keys: Arc<std::sync::Mutex<Vec<String>>>,
        lose_write_ack: bool,
        read_behavior: ExternalReadBehavior,
    }
    #[async_trait::async_trait]
    impl crate::CompensationIdempotencyStore for ExternalRecoveryMarker {
        async fn was_compensated(&self, key: &str) -> anyhow::Result<bool> {
            let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            self.keys.lock().unwrap().push(key.to_string());
            match (self.read_behavior, read) {
                (ExternalReadBehavior::FailSecond, 2) => {
                    Err(anyhow::anyhow!("private readback failure"))
                }
                (ExternalReadBehavior::PendingSecond, 2) => std::future::pending().await,
                (ExternalReadBehavior::DropPanicSecond, 2) => PanicOnDropPending.await,
                (ExternalReadBehavior::PollPanicSecond, 2) => panic!("private store poll panic"),
                (ExternalReadBehavior::BlockingPastDeadlineSecond, 2) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    Ok(self.applied.load(Ordering::SeqCst))
                }
                _ => Ok(self.applied.load(Ordering::SeqCst)),
            }
        }
        async fn mark_compensated(&self, key: &str) -> anyhow::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.keys.lock().unwrap().push(key.to_string());
            self.applied.store(true, Ordering::SeqCst);
            if self.lose_write_ack {
                Err(anyhow::anyhow!("private lost marker acknowledgement"))
            } else {
                Ok(())
            }
        }
    }
    struct ExternalRecoveryFixture {
        _supervisor: StreamSupervisor,
        invocation: SupervisedStreamInvocation<u32, u32, String>,
        journal: crate::SagaRollbackJournal,
        hook_calls: Arc<AtomicUsize>,
        marker_reads: Arc<AtomicUsize>,
        marker_writes: Arc<AtomicUsize>,
        marker_keys: Arc<std::sync::Mutex<Vec<String>>>,
    }
    async fn external_recovery_fixture(
        hook_succeeds: bool,
        lose_write_ack: bool,
        read_behavior: ExternalReadBehavior,
        include_marker: bool,
        prior_local: bool,
    ) -> ExternalRecoveryFixture {
        let supervisor = StreamSupervisor::new(1);
        let trace_id = "external-readback-fixture".to_string();
        let captured = Arc::new(std::sync::Mutex::new(None));
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let marker_reads = Arc::new(AtomicUsize::new(0));
        let marker_writes = Arc::new(AtomicUsize::new(0));
        let marker_keys = Arc::new(std::sync::Mutex::new(Vec::new()));
        let applied = Arc::new(AtomicBool::new(false));
        let hook: Arc<dyn crate::CompensationHook> = Arc::new(ExternalRecoveryHook {
            calls: hook_calls.clone(),
            succeed: hook_succeeds,
        });
        let marker: Arc<dyn crate::CompensationIdempotencyStore> =
            Arc::new(ExternalRecoveryMarker {
                applied,
                reads: marker_reads.clone(),
                writes: marker_writes.clone(),
                keys: marker_keys.clone(),
                lose_write_ack,
                read_behavior,
            });
        let mut bus = ranvier_core::Bus::new();
        bus.insert(crate::PersistenceTraceId::new("external-readback-fixture"));
        bus.insert(crate::CompensationHandle::from_arc(hook));
        if include_marker {
            bus.insert(crate::CompensationIdempotencyHandle::from_arc(marker));
        }
        bus.insert(crate::PersistenceHandle::from_store(
            crate::InMemoryPersistenceStore::new(),
        ));
        let circuit = crate::Axon::<u32, u32, String>::start("external-readback-fixture")
            .then(CaptureExternalJournal {
                trace_id,
                captured: captured.clone(),
                prior_local,
            })
            .then(ExternalFailure)
            .then_stream(Finite);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), bus))
            .unwrap();
        let prepared = ticket
            .prepare(
                &circuit,
                context,
                271,
                std::time::Duration::from_secs(2),
                None,
            )
            .unwrap_or_else(|_| panic!("external recovery setup rejected"));
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let _ = tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .expect("external recovery fixture initialization timed out");
        while tokio::time::timeout_at(deadline, invocation.items.recv())
            .await
            .expect("external recovery fixture delivery timed out")
            .is_some()
        {}
        let journal = captured
            .lock()
            .unwrap()
            .clone()
            .expect("external journal was not captured");
        ExternalRecoveryFixture {
            _supervisor: supervisor,
            invocation,
            journal,
            hook_calls,
            marker_reads,
            marker_writes,
            marker_keys,
        }
    }
    #[derive(Clone)]
    struct InjectedSagaState {
        node_id: String,
    }
    #[async_trait::async_trait]
    impl ranvier_core::streaming::StreamingTransition<u32> for InjectedSagaState {
        type Item = u32;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: u32,
            _: &(),
            bus: &mut ranvier_core::Bus,
        ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String>
        {
            bus.insert(ranvier_core::saga::SagaPolicy::Enabled);
            let mut stack = ranvier_core::saga::SagaStack::new();
            stack.push(
                self.node_id.clone(),
                "private-label".into(),
                serde_json::to_vec(&input).unwrap(),
            );
            bus.insert(stack);
            Ok(Box::pin(futures_util::stream::iter([input])))
        }
    }
    #[tokio::test]
    async fn public_invocation_transfers_original_item_and_report_at_capacity_one() {
        let supervisor = StreamSupervisor::new(1);
        let circuit = crate::Axon::<u32, u32, String>::start("public-owned")
            .then(Prefix)
            .then_stream(Finite);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = match ticket.prepare(
            &circuit,
            context,
            233,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(_) => panic!("setup rejected"),
        };
        let parent = ranvier_core::CancellationToken::new();
        let mut invocation = prepared
            .start(parent.clone())
            .await
            .unwrap_or_else(|e| panic!("{}", e.signal));
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            invocation.readiness.initialized(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), invocation.items.recv())
                .await
                .unwrap(),
            Some(233)
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), invocation.items.recv())
                .await
                .unwrap(),
            None
        );
        let lease = invocation.report().clone();
        let report = invocation
            .report()
            .take_report_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            matches!(report, crate::streaming_report_handle::SupervisedStreamCompletion::Report(
            crate::StreamInitializationReport::Ready { value, .. })
            if matches!(value.consumption, Ok(crate::StreamConsumptionTerminal::Completed)))
        );
        let live = lease
            .live_recovery_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!live.saga_stack_nonempty);
        let assessment = lease.assessment().unwrap().unwrap();
        assert_eq!(
            assessment.frame,
            crate::streaming_report_handle::StreamFrameAssessment::Reported
        );
        assert_eq!(
            assessment.initialization,
            crate::streaming_report_handle::StreamInitializationAssessment::Ready
        );
        assert_eq!(
            assessment.consumption,
            Some(crate::streaming_report_handle::StreamConsumptionAssessment::Completed)
        );
        assert_eq!(
            assessment.prefix_durability,
            crate::streaming_report_handle::StreamPrefixDurability::Unpersisted
        );
        assert_eq!(
            assessment.cleanup,
            crate::streaming_report_handle::StreamCleanupAssessment::NotReported
        );
        assert_eq!(
            lease.retirement_evidence(),
            crate::streaming_report_handle::StreamRetirementEvidence::RequiresRecoveryReview
        );
        assert_eq!(invocation.report().assessment().unwrap(), Some(assessment));
        assert!(supervisor.reserve().is_err());
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let other = lease.clone();
        let (first, second) = tokio::join!(
            lease.retire_completed_local_until(deadline),
            other.retire_completed_local_until(deadline)
        );
        assert_eq!(first, Ok(()));
        assert_eq!(second, Ok(()));
        let root_slot = supervisor.root.reserve_stream_slot().unwrap();
        drop(root_slot);
        assert_eq!(supervisor.admission.in_use(), Ok(1));
        drop(invocation);
        assert!(!parent.is_cancelled());
        assert_eq!(lease.retire_completed_local_until(deadline).await, Ok(()));
        drop(other);
        drop(lease);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
    }
    #[tokio::test]
    async fn explicit_local_recovery_joins_original_report_moves_stack_and_uses_frozen_handler() {
        let supervisor = StreamSupervisor::new(1);
        let compensations = Arc::new(AtomicUsize::new(0));
        let circuit = crate::Axon::<u32, u32, String>::start("owned-local-recovery")
            .then_compensated(Prefix, CountUndo(compensations.clone()))
            .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
            .then_stream(Finite);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = ticket
            .prepare(
                &circuit,
                context,
                251,
                std::time::Duration::from_secs(1),
                None,
            )
            .unwrap_or_else(|_| panic!("setup rejected"));
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            Some(251)
        );
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            None
        );
        let report = invocation.report();
        assert_eq!(
            report
                .reconcile_next_local_compensation_until(deadline)
                .await,
            Ok(crate::SagaRollbackOutcome::Completed)
        );
        assert_eq!(compensations.load(Ordering::SeqCst), 1);
        let live = report.live_recovery_until(deadline).await.unwrap();
        assert!(!live.saga_stack_nonempty);
        assert!(live.journal_present && live.recovery_pending);
        assert_eq!(
            report
                .reconcile_next_local_compensation_until(deadline)
                .await,
            Err(crate::streaming_report_handle::StreamLocalRecoveryError::RecoveryBlocked)
        );
        let original = report.take_report_until(deadline).await.unwrap();
        assert!(
            matches!(original, crate::streaming_report_handle::SupervisedStreamCompletion::Report(
            crate::StreamInitializationReport::Ready { value, .. })
            if matches!(value.consumption, Ok(crate::StreamConsumptionTerminal::Completed)))
        );
        assert_eq!(
            report.retire_completed_local_until(deadline).await,
            Err(crate::streaming_report_handle::StreamLocalRetirementError::RecoveryReviewRequired)
        );
    }
    #[tokio::test]
    async fn missing_local_handler_is_zero_call_and_keeps_the_first_recovery_row_blocking() {
        #[derive(Clone)]
        struct PendingLocalTask;
        #[async_trait::async_trait]
        impl ranvier_core::streaming::StreamingTransition<u32> for PendingLocalTask {
            type Item = u32;
            type Error = String;
            type Resources = ();
            async fn run_stream(
                &self,
                input: u32,
                _: &(),
                bus: &mut ranvier_core::Bus,
            ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String>
            {
                bus.insert(ranvier_core::saga::SagaPolicy::Enabled);
                let mut stack = ranvier_core::saga::SagaStack::new();
                stack.push(
                    "unknown-node".into(),
                    "private-label".into(),
                    serde_json::to_vec(&input).unwrap(),
                );
                bus.insert(stack);
                Ok(Box::pin(futures_util::stream::iter([input])))
            }
        }
        let supervisor = StreamSupervisor::new(1);
        let circuit = crate::Axon::<u32, u32, String>::start("missing-local-handler")
            .then(Prefix)
            .with_saga_policy(ranvier_core::saga::SagaPolicy::Enabled)
            .then_stream(PendingLocalTask);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = ticket
            .prepare(
                &circuit,
                context,
                257,
                std::time::Duration::from_secs(1),
                None,
            )
            .unwrap_or_else(|_| panic!("setup rejected"));
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        let _ = invocation.items.recv().await;
        let _ = invocation.items.recv().await;
        assert_eq!(
            invocation
                .report()
                .reconcile_next_local_compensation_until(deadline)
                .await,
            Ok(crate::SagaRollbackOutcome::MissingHandler)
        );
        assert_eq!(
            invocation
                .report()
                .reconcile_next_local_compensation_until(deadline)
                .await,
            Err(crate::streaming_report_handle::StreamLocalRecoveryError::RecoveryBlocked)
        );
        let live = invocation
            .report()
            .live_recovery_until(deadline)
            .await
            .unwrap();
        assert!(!live.saga_stack_nonempty && live.journal_present && live.recovery_pending);
    }
    #[tokio::test]
    async fn caller_bus_policy_mutation_cannot_authorize_original_graph_compensation() {
        let supervisor = StreamSupervisor::new(1);
        let compensations = Arc::new(AtomicUsize::new(0));
        let prefix = crate::Axon::<u32, u32, String>::start("caller-policy-mutation")
            .then_compensated(Prefix, CountUndo(compensations.clone()));
        let node_id = prefix
            .schematic
            .nodes
            .iter()
            .find(|node| node.compensation_node_id.is_some())
            .expect("compensated node exists")
            .id
            .clone();
        // The original graph policy is Disabled. A streaming callback can alter
        // its caller Bus after the prefix ran, but that cannot grant recovery.
        let circuit = prefix.then_stream(InjectedSagaState { node_id });
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = ticket
            .prepare(
                &circuit,
                context,
                251,
                std::time::Duration::from_secs(1),
                None,
            )
            .unwrap_or_else(|_| panic!("setup rejected"));
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        let _ = invocation.items.recv().await;
        let _ = invocation.items.recv().await;
        assert_eq!(
            invocation
                .report()
                .reconcile_next_local_compensation_until(deadline)
                .await,
            Err(crate::streaming_report_handle::StreamLocalRecoveryError::RecoveryBlocked)
        );
        assert_eq!(compensations.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn dynamic_saga_policy_is_frozen_for_the_admitted_owner() {
        let supervisor = StreamSupervisor::new(1);
        let compensations = Arc::new(AtomicUsize::new(0));
        let (updates, policy) =
            ranvier_core::prelude::DynamicPolicy::new(ranvier_core::saga::SagaPolicy::Disabled);
        let prefix = crate::Axon::<u32, u32, String>::start("frozen-dynamic-policy")
            .then_compensated(Prefix, CountUndo(compensations.clone()))
            .with_dynamic_saga_policy(policy);
        let node_id = prefix
            .schematic
            .nodes
            .iter()
            .find(|node| node.compensation_node_id.is_some())
            .expect("compensated node exists")
            .id
            .clone();
        let circuit = prefix.then_stream(InjectedSagaState { node_id });
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = ticket
            .prepare(
                &circuit,
                context,
                263,
                std::time::Duration::from_secs(1),
                None,
            )
            .unwrap_or_else(|_| panic!("setup rejected"));
        // Owner admission captured Disabled. A later hot reload cannot grant
        // the retained frame a handler authority it did not start with.
        updates.send_replace(ranvier_core::saga::SagaPolicy::Enabled);
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        let _ = invocation.items.recv().await;
        let _ = invocation.items.recv().await;
        assert_eq!(
            invocation
                .report()
                .reconcile_next_local_compensation_until(deadline)
                .await,
            Err(crate::streaming_report_handle::StreamLocalRecoveryError::RecoveryBlocked)
        );
        assert_eq!(compensations.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn external_readback_confirms_marker_without_replay_or_journal_sealing() {
        use crate::streaming_report_handle::{StreamExternalReadback, StreamExternalRecoveryError};
        let fixture =
            external_recovery_fixture(true, true, ExternalReadBehavior::Normal, true, false).await;
        let report = fixture.invocation.report();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let original = report.take_report_until(deadline).await.unwrap();
        let original_outcome = match original {
            crate::streaming_report_handle::SupervisedStreamCompletion::Report(
                crate::StreamInitializationReport::Rejected { prefix },
            ) => prefix.cleanup().unwrap().steps[0].outcome,
            _ => panic!("expected original rejected prefix report"),
        };
        assert_eq!(original_outcome, crate::SagaRollbackOutcome::Fault);
        assert_eq!(
            report
                .readback_next_external_compensation_until(deadline)
                .await,
            Ok(StreamExternalReadback::MarkerPresent)
        );
        assert_eq!(fixture.marker_reads.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.marker_writes.load(Ordering::SeqCst), 1);
        let keys = fixture.marker_keys.lock().unwrap().clone();
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0], keys[1]);
        assert_eq!(keys[1], keys[2]);
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
        let snapshot = fixture.journal.snapshot();
        assert_eq!(
            snapshot.steps[0].outcome,
            crate::SagaRollbackOutcome::Completed
        );
        assert!(!snapshot.finished, "readback must not seal the journal");
        assert!(
            report
                .live_recovery_until(deadline)
                .await
                .unwrap()
                .recovery_pending
        );
        assert_eq!(
            report
                .readback_next_external_compensation_until(deadline)
                .await,
            Err(StreamExternalRecoveryError::RecoveryBlocked)
        );
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .invocation
                .report()
                .retire_completed_local_until(deadline)
                .await,
            Err(crate::streaming_report_handle::StreamLocalRetirementError::RecoveryReviewRequired)
        );
    }
    #[tokio::test]
    async fn negative_external_readback_retains_uncertainty_and_never_reinvokes_hook() {
        use crate::streaming_report_handle::StreamExternalReadback;
        let fixture =
            external_recovery_fixture(false, false, ExternalReadBehavior::Normal, true, false)
                .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            fixture
                .invocation
                .report()
                .readback_next_external_compensation_until(deadline)
                .await,
            Ok(StreamExternalReadback::MarkerAbsent)
        );
        assert_eq!(fixture.marker_reads.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.marker_writes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.journal.snapshot().steps[0].outcome,
            crate::SagaRollbackOutcome::Fault
        );
        assert!(fixture.journal.has_unresolved_tasks());
    }
    #[tokio::test]
    async fn failed_or_timed_out_external_readback_preserves_capability_for_later_read() {
        use crate::streaming_report_handle::{
            StreamExternalReadback, StreamExternalRecoveryError, StreamReportWaitError,
        };
        for behavior in [
            ExternalReadBehavior::FailSecond,
            ExternalReadBehavior::PendingSecond,
        ] {
            let fixture = external_recovery_fixture(true, true, behavior, true, false).await;
            let report = fixture.invocation.report();
            let short_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
            let first = report
                .readback_next_external_compensation_until(short_deadline)
                .await;
            if matches!(behavior, ExternalReadBehavior::FailSecond) {
                assert_eq!(first, Ok(StreamExternalReadback::ReadUnavailable));
            } else {
                assert_eq!(
                    first,
                    Err(StreamExternalRecoveryError::Wait(
                        StreamReportWaitError::TimedOut
                    ))
                );
            }
            assert_eq!(
                fixture.journal.snapshot().steps[0].outcome,
                crate::SagaRollbackOutcome::Fault
            );
            assert!(fixture.journal.has_unresolved_tasks());
            assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
            let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            assert_eq!(
                report
                    .readback_next_external_compensation_until(retry_deadline)
                    .await,
                Ok(StreamExternalReadback::MarkerPresent)
            );
            assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                fixture.journal.snapshot().steps[0].outcome,
                crate::SagaRollbackOutcome::Completed
            );
            assert!(!fixture.journal.snapshot().finished);
        }
    }
    #[tokio::test]
    async fn external_readback_contains_panicking_store_future_drop() {
        use crate::streaming_report_handle::{
            StreamExternalReadback, StreamExternalRecoveryError, StreamReportWaitError,
        };
        use futures_util::FutureExt as _;
        let fixture = external_recovery_fixture(
            true,
            true,
            ExternalReadBehavior::DropPanicSecond,
            true,
            false,
        )
        .await;
        let report = fixture.invocation.report();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        let result = std::panic::AssertUnwindSafe(
            report.readback_next_external_compensation_until(deadline),
        )
        .catch_unwind()
        .await;
        assert!(
            result.is_ok(),
            "the timed-out store future's Drop panic must not escape"
        );
        assert_eq!(
            result.unwrap(),
            Err(StreamExternalRecoveryError::Wait(
                StreamReportWaitError::TimedOut
            ))
        );
        assert_eq!(
            fixture.journal.snapshot().steps[0].outcome,
            crate::SagaRollbackOutcome::Fault
        );
        let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            report
                .readback_next_external_compensation_until(retry_deadline)
                .await,
            Ok(StreamExternalReadback::MarkerPresent)
        );
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn external_readback_contains_store_poll_panic_and_releases_claim() {
        use crate::streaming_report_handle::StreamExternalReadback;
        use futures_util::FutureExt as _;
        let fixture = external_recovery_fixture(
            true,
            true,
            ExternalReadBehavior::PollPanicSecond,
            true,
            false,
        )
        .await;
        let report = fixture.invocation.report();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = std::panic::AssertUnwindSafe(
            report.readback_next_external_compensation_until(deadline),
        )
        .catch_unwind()
        .await;
        assert!(result.is_ok(), "a marker store poll panic must not escape");
        assert_eq!(result.unwrap(), Ok(StreamExternalReadback::ReadUnavailable));
        assert_eq!(
            fixture.journal.snapshot().steps[0].outcome,
            crate::SagaRollbackOutcome::Fault
        );
        let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            report
                .readback_next_external_compensation_until(retry_deadline)
                .await,
            Ok(StreamExternalReadback::MarkerPresent)
        );
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn external_readback_does_not_settle_a_late_positive_marker() {
        use crate::streaming_report_handle::{
            StreamExternalReadback, StreamExternalRecoveryError, StreamReportWaitError,
        };
        let fixture = external_recovery_fixture(
            true,
            true,
            ExternalReadBehavior::BlockingPastDeadlineSecond,
            true,
            false,
        )
        .await;
        let report = fixture.invocation.report();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        assert_eq!(
            report
                .readback_next_external_compensation_until(deadline)
                .await,
            Err(StreamExternalRecoveryError::Wait(
                StreamReportWaitError::TimedOut
            ))
        );
        assert_eq!(
            fixture.journal.snapshot().steps[0].outcome,
            crate::SagaRollbackOutcome::Fault
        );
        assert!(fixture.journal.has_unresolved_tasks());
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
        let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            report
                .readback_next_external_compensation_until(retry_deadline)
                .await,
            Ok(StreamExternalReadback::MarkerPresent)
        );
    }
    #[tokio::test]
    async fn pending_external_readback_releases_owner_context_and_serializes_retry() {
        use crate::streaming_report_handle::{
            StreamExternalReadback, StreamExternalRecoveryError, StreamReportWaitError,
        };
        let fixture =
            external_recovery_fixture(true, true, ExternalReadBehavior::PendingSecond, true, false)
                .await;
        let report = fixture.invocation.report();
        let first_report = report.clone();
        let first = tokio::spawn(async move {
            first_report
                .readback_next_external_compensation_until(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while fixture.marker_reads.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first marker readback did not start");

        let probe_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(100);
        assert!(
            report.live_recovery_until(probe_deadline).await.is_ok(),
            "a marker read should not hold the original owner context while awaiting storage"
        );
        let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        assert_eq!(
            report
                .readback_next_external_compensation_until(retry_deadline)
                .await,
            Err(StreamExternalRecoveryError::Wait(
                StreamReportWaitError::TimedOut
            ))
        );
        assert_eq!(
            fixture.marker_reads.load(Ordering::SeqCst),
            2,
            "the serialized retry must not start a second store read"
        );

        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(
            fixture.journal.snapshot().steps[0].outcome,
            crate::SagaRollbackOutcome::Fault
        );
        let final_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            report
                .readback_next_external_compensation_until(final_deadline)
                .await,
            Ok(StreamExternalReadback::MarkerPresent)
        );
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn external_readback_cannot_pass_an_earlier_local_journal_step() {
        use crate::streaming_report_handle::StreamExternalRecoveryError;
        let fixture =
            external_recovery_fixture(true, true, ExternalReadBehavior::Normal, true, true).await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            fixture
                .invocation
                .report()
                .readback_next_external_compensation_until(deadline)
                .await,
            Err(StreamExternalRecoveryError::RecoveryBlocked)
        );
        let snapshot = fixture.journal.snapshot();
        assert_eq!(snapshot.steps.len(), 2);
        assert_eq!(
            snapshot.steps[0].outcome,
            crate::SagaRollbackOutcome::NotInvoked
        );
        assert_eq!(snapshot.steps[1].outcome, crate::SagaRollbackOutcome::Fault);
        assert_eq!(fixture.marker_reads.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn missing_external_marker_store_stays_unavailable_without_hook_retry() {
        use crate::streaming_report_handle::StreamExternalReadback;
        let fixture =
            external_recovery_fixture(false, false, ExternalReadBehavior::Normal, false, false)
                .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            fixture
                .invocation
                .report()
                .readback_next_external_compensation_until(deadline)
                .await,
            Ok(StreamExternalReadback::ReadUnavailable)
        );
        assert_eq!(fixture.marker_reads.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.marker_writes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.hook_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.journal.snapshot().steps[0].outcome,
            crate::SagaRollbackOutcome::Fault
        );
    }
    #[derive(Clone)]
    struct LocalState(Option<bool>);
    #[async_trait::async_trait]
    impl ranvier_core::streaming::StreamingTransition<u32> for LocalState {
        type Item = u32;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: u32,
            _: &(),
            bus: &mut ranvier_core::Bus,
        ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String>
        {
            match self.0 {
                Some(true) => {
                    let journal = crate::SagaRollbackJournal::new("private retained journal");
                    journal.finish();
                    bus.insert(journal);
                }
                Some(false) => {
                    let mut stack = ranvier_core::saga::SagaStack::new();
                    stack.push("private id".into(), "private label".into(), vec![1, 2, 3]);
                    bus.insert(stack);
                }
                None => {}
            }
            Ok(Box::pin(futures_util::stream::iter([input])))
        }
    }
    async fn completed_local_fixture(
        supervisor: &StreamSupervisor,
        state: Option<bool>,
    ) -> SupervisedStreamInvocation<u32, u32, String> {
        let circuit = crate::Axon::<u32, u32, String>::start("local-retirement")
            .then(Prefix)
            .then_stream(LocalState(state));
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = match ticket.prepare(
            &circuit,
            context,
            359,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(_) => panic!("fixture setup rejected"),
        };
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{}", e.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            Some(359)
        );
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            None
        );
        invocation.report().settle_until(deadline).await.unwrap();
        invocation
    }
    #[tokio::test]
    async fn completed_local_retirement_holds_any_journal_and_nonempty_stack() {
        use crate::streaming_report_handle::StreamLocalRetirementError as Error;
        for with_journal in [false, true] {
            let supervisor = StreamSupervisor::new(1);
            let invocation = completed_local_fixture(&supervisor, Some(with_journal)).await;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            assert_eq!(
                invocation
                    .report()
                    .retire_completed_local_until(deadline)
                    .await,
                Err(Error::RecoveryReviewRequired)
            );
            assert!(supervisor.root.reserve_stream_slot().is_err());
            assert_eq!(supervisor.admission.in_use(), Ok(1));
        }
    }
    #[tokio::test]
    async fn completed_local_retirement_expiry_and_missing_root_never_claim_success() {
        use crate::streaming_report_handle::{
            StreamLocalRetirementError as Error, StreamReportWaitError,
        };
        let supervisor = StreamSupervisor::new(1);
        let invocation = completed_local_fixture(&supervisor, None).await;
        assert_eq!(
            invocation
                .report()
                .retire_completed_local_until(tokio::time::Instant::now())
                .await,
            Err(Error::Wait(StreamReportWaitError::TimedOut))
        );
        assert!(supervisor.root.reserve_stream_slot().is_err());
        drop(supervisor);
        for _ in 0..2 {
            assert_eq!(
                invocation
                    .report()
                    .retire_completed_local_until(
                        tokio::time::Instant::now() + std::time::Duration::from_secs(5)
                    )
                    .await,
                Err(Error::CustodyReleased)
            );
        }
    }
    #[tokio::test]
    async fn completed_local_retirement_after_close_preserves_original_drain_lease() {
        let supervisor = StreamSupervisor::new(1);
        let invocation = completed_local_fixture(&supervisor, None).await;
        let report = invocation.report().clone();
        let mut family = supervisor.close_for_drain().unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        family.settle_until(deadline).await;
        assert_eq!(report.retire_completed_local_until(deadline).await, Ok(()));
        assert!(matches!(
            report.take_report_until(deadline).await.unwrap(),
            crate::streaming_report_handle::SupervisedStreamCompletion::Report(
                crate::StreamInitializationReport::Ready { .. }
            )
        ));
        family.settle_until(deadline).await;
        assert_eq!(
            family.statuses()[0].1,
            StreamDrainStatus::ReportAlreadyTransferred
        );
        drop(invocation);
        drop(report);
        assert_eq!(supervisor.admission.in_use(), Ok(1));
        drop(family);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
    }
    #[tokio::test]
    async fn public_start_rejection_and_setup_rejection_keep_original_custody() {
        for closed_after_prepare in [false, true] {
            let supervisor = StreamSupervisor::new(1);
            let circuit = crate::Axon::<u32, u32, String>::start("public-rejected")
                .then(Prefix)
                .then_stream(Finite);
            let (ticket, context) = supervisor
                .reserve()
                .unwrap()
                .with_context(|| {
                    let mut bus = ranvier_core::Bus::new();
                    bus.insert(239u32);
                    OwnedStreamContext::try_new(Arc::new(()), bus)
                })
                .unwrap();
            if !closed_after_prepare {
                supervisor.close().unwrap();
            }
            let prepared = match ticket.prepare(
                &circuit,
                context,
                241,
                std::time::Duration::from_secs(1),
                None,
            ) {
                Err(StreamPrepareError::Setup { input, context, .. }) if !closed_after_prepare => {
                    assert_eq!(input, 241);
                    assert_eq!(*context.into_parts().1.read::<u32>().unwrap(), 239);
                    assert_eq!(supervisor.admission.in_use(), Ok(0));
                    continue;
                }
                Ok(prepared) if closed_after_prepare => prepared,
                _ => panic!("wrong setup disposition"),
            };
            supervisor.close().unwrap();
            let rejected = match prepared.start(ranvier_core::CancellationToken::new()).await {
                Err(rejected) => rejected,
                Ok(_) => panic!("closed root admitted"),
            };
            assert_eq!(rejected.signal, "execution.nested_custody_admission_closed");
            let report = rejected
                .report()
                .take_report_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .await
                .unwrap();
            assert!(matches!(
                report,
                crate::streaming_report_handle::SupervisedStreamCompletion::RejectedBeforeStart
            ));
            rejected
                .retire_unstarted_until(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
            rejected
                .retire_unstarted_until(
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                )
                .await
                .unwrap();
            assert_eq!(
                rejected.report().retirement_evidence(),
                crate::streaming_report_handle::StreamRetirementEvidence::RejectedBeforeStart
            );
            assert_eq!(supervisor.admission.in_use(), Ok(1));
            drop(rejected);
            assert_eq!(supervisor.admission.in_use(), Ok(0));
        }
    }
    #[tokio::test]
    async fn public_invocation_drop_cancels_actual_pending_stream_but_not_parent() {
        #[derive(Clone)]
        struct Pending;
        #[async_trait::async_trait]
        impl ranvier_core::streaming::StreamingTransition<u32> for Pending {
            type Item = u32;
            type Error = String;
            type Resources = ();
            async fn run_stream(
                &self,
                _: u32,
                _: &(),
                _: &mut ranvier_core::Bus,
            ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String>
            {
                Ok(Box::pin(futures_util::stream::pending()))
            }
        }
        let supervisor = StreamSupervisor::new(1);
        let circuit = crate::Axon::<u32, u32, String>::start("public-drop")
            .then(Prefix)
            .then_stream(Pending);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = match ticket.prepare(
            &circuit,
            context,
            251,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(_) => panic!("setup rejected"),
        };
        let parent = ranvier_core::CancellationToken::new();
        let mut invocation = prepared
            .start(parent.clone())
            .await
            .unwrap_or_else(|e| panic!("{}", e.signal));
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            invocation.readiness.initialized(),
        )
        .await
        .unwrap()
        .unwrap();
        let report = invocation.report().clone();
        drop(invocation);
        assert!(!parent.is_cancelled());
        let terminal = report
            .take_report_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        match terminal {
            crate::streaming_report_handle::SupervisedStreamCompletion::Report(
                crate::StreamInitializationReport::Ready { value, .. },
            ) => {
                let context = match value.consumption {
                    Ok(crate::StreamConsumptionTerminal::Cancelled(context))
                    | Err(crate::streaming_report_handle::SupervisedStreamFailure::Cancelled(
                        context,
                    )) => context,
                    _ => panic!("actual cancellation terminal lost"),
                };
                assert_eq!(
                    context.reason,
                    ranvier_core::CancellationReason::ClientDisconnected
                );
            }
            _ => panic!("original ready report lost"),
        }
        assert!(!parent.is_cancelled());
        assert_eq!(supervisor.admission.in_use(), Ok(1));
        let admission = supervisor.admission.clone();
        drop(supervisor);
        assert_eq!(admission.in_use(), Ok(1));
        assert!(matches!(
            report
                .take_report_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
                .await,
            Err(crate::streaming_report_handle::StreamReportWaitError::AlreadyTaken)
        ));
        drop(report);
        assert_eq!(admission.in_use(), Ok(0));
    }
    #[test]
    fn invalid_public_buffer_returns_original_context_before_owner_creation() {
        let supervisor = StreamSupervisor::new(1);
        let circuit = crate::Axon::<u32, u32, String>::start("public-invalid-buffer")
            .then(Prefix)
            .then_stream(Finite)
            .with_buffer_size(0);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| {
                let mut bus = ranvier_core::Bus::new();
                bus.insert(257u32);
                OwnedStreamContext::try_new(Arc::new(()), bus)
            })
            .unwrap();
        match ticket.prepare(
            &circuit,
            context,
            263,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Err(StreamPrepareError::Setup { input, context, .. }) => {
                assert_eq!(input, 263);
                assert_eq!(*context.into_parts().1.read::<u32>().unwrap(), 257);
            }
            _ => panic!("invalid buffer created owner"),
        }
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
    }
    #[tokio::test]
    async fn rejected_frame_retirement_refunds_actual_record_after_timeout_and_report_transfer() {
        use crate::streaming_report_handle::{StreamReportWaitError, StreamRetirementEvidence};
        let root = CustodyStore::new(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let owner: crate::nested_custody::Capability = calls.clone();
        let observed = calls.clone();
        let mut prepared =
            crate::managed_stream_task::PreparedStreamTask::new_owned(owner.clone(), async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Err::<
                    crate::StreamInitializationReport<
                        u32,
                        crate::streaming_recovery::StreamReadyResult<()>,
                        String,
                    >,
                    _,
                >(crate::streaming_recovery::StreamInvocationAlreadyStarted)
            });
        let journal = crate::SagaRollbackJournal::new("rejected-retirement-fixture");
        let observer =
            tokio::sync::watch::channel(crate::streaming_recovery::SupervisorObservation {
                phase: crate::streaming_recovery::SupervisorPhase::NotStarted,
                disposition: None,
                cleanup_complete: None,
                cleanup_timed_out: false,
                custody: crate::streaming_recovery::RecoveryCustody::Held,
            })
            .1;
        let receipt = root
            .client()
            .reserve_stream(owner.clone(), &journal, Some(prepared.holder()), observer)
            .unwrap();
        prepared.reject_start().unwrap();
        let report =
            crate::streaming_report_handle::StreamReportHandle::new(prepared.holder(), owner);
        let clone = report.clone();
        let rejected = StreamStartRejected {
            signal: "fixture.rejected_start",
            report,
            _custody: Some(receipt),
        };
        let holder = prepared.holder();
        let locked = holder.lock().await;
        assert_eq!(
            rejected
                .retire_unstarted_until(tokio::time::Instant::now())
                .await,
            Err(StreamRetirementError::Wait(StreamReportWaitError::TimedOut))
        );
        assert_eq!(
            clone.retirement_evidence(),
            StreamRetirementEvidence::NotObserved
        );
        assert!(root.reserve_stream_slot().is_err());
        drop(locked);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let original = clone.take_report_until(deadline).await.unwrap();
        assert!(matches!(
            original,
            crate::streaming_report_handle::SupervisedStreamCompletion::RejectedBeforeStart
        ));
        assert_eq!(
            rejected.report().retirement_evidence(),
            StreamRetirementEvidence::RejectedBeforeStart
        );
        rejected.retire_unstarted_until(deadline).await.unwrap();
        rejected.retire_unstarted_until(deadline).await.unwrap();
        assert!(root.reserve_stream_slot().is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn executed_frame_cannot_use_unstarted_retirement_after_report_transfer() {
        use crate::streaming_report_handle::StreamRetirementEvidence;
        let supervisor = StreamSupervisor::new(1);
        let circuit = crate::Axon::<u32, u32, String>::start("executed-retirement")
            .then(Prefix)
            .then_stream(Finite);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = match ticket.prepare(
            &circuit,
            context,
            269,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(_) => panic!("setup rejected"),
        };
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{}", e.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            Some(269)
        );
        let original = invocation
            .report()
            .take_report_until(deadline)
            .await
            .unwrap();
        assert!(matches!(
            original,
            crate::streaming_report_handle::SupervisedStreamCompletion::Report(_)
        ));
        let wrong_authority = StreamStartRejected {
            signal: "fixture.executed",
            report: invocation.report().clone(),
            _custody: Some(invocation._custody.clone()),
        };
        assert_eq!(
            wrong_authority.retire_unstarted_until(deadline).await,
            Err(StreamRetirementError::RecoveryReviewRequired)
        );
        assert_eq!(
            invocation.report().retirement_evidence(),
            StreamRetirementEvidence::RequiresRecoveryReview
        );
        assert!(supervisor.reserve().is_err());
    }
    #[tokio::test]
    async fn public_drain_retains_transferred_report_status_and_blocks_reserved_late_start() {
        let supervisor = StreamSupervisor::new(2);
        let ticket = supervisor.reserve().unwrap();
        let circuit = crate::Axon::<u32, u32, String>::start("drain-transferred")
            .then(Prefix)
            .then_stream(Finite);
        let (active, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = match active.prepare(
            &circuit,
            context,
            293,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(_) => panic!("setup rejected"),
        };
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{}", e.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        assert_eq!(
            tokio::time::timeout_at(deadline, invocation.items.recv())
                .await
                .unwrap(),
            Some(293)
        );
        let original = invocation
            .report()
            .take_report_until(deadline)
            .await
            .unwrap();
        assert!(matches!(
            original,
            crate::streaming_report_handle::SupervisedStreamCompletion::Report(_)
        ));
        let mut drain = supervisor.close_for_drain().unwrap();
        assert_eq!(drain.unfinalized_reservations(), 1);
        drain.settle_until(deadline).await;
        assert_eq!(drain.statuses().len(), 1);
        assert_eq!(
            drain.statuses()[0].1,
            StreamDrainStatus::ReportAlreadyTransferred
        );
        let calls = AtomicUsize::new(0);
        assert!(
            ticket
                .with_context(|| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())
                })
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn public_drain_drop_timeout_retry_keeps_actual_report_and_owner() {
        use futures_util::FutureExt;
        #[derive(Clone)]
        struct Pending;
        #[async_trait::async_trait]
        impl ranvier_core::streaming::StreamingTransition<u32> for Pending {
            type Item = u32;
            type Error = String;
            type Resources = ();
            async fn run_stream(
                &self,
                _: u32,
                _: &(),
                _: &mut ranvier_core::Bus,
            ) -> Result<std::pin::Pin<Box<dyn futures_core::Stream<Item = u32> + Send>>, String>
            {
                Ok(Box::pin(futures_util::stream::pending()))
            }
        }
        let supervisor = StreamSupervisor::new(1);
        let admission = supervisor.admission.clone();
        let circuit = crate::Axon::<u32, u32, String>::start("drain-retry")
            .then(Prefix)
            .then_stream(Pending);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new()))
            .unwrap();
        let prepared = match ticket.prepare(
            &circuit,
            context,
            307,
            std::time::Duration::from_secs(1),
            None,
        ) {
            Ok(prepared) => prepared,
            Err(_) => panic!("setup rejected"),
        };
        let mut invocation = prepared
            .start(ranvier_core::CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{}", e.signal));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        tokio::time::timeout_at(deadline, invocation.readiness.initialized())
            .await
            .unwrap()
            .unwrap();
        let mut drain = supervisor.close_for_drain().unwrap();
        assert_eq!(drain.statuses()[0].1, StreamDrainStatus::Pending);
        assert!(drain.settle_until(deadline).now_or_never().is_none());
        assert_eq!(drain.statuses()[0].1, StreamDrainStatus::Pending);
        drain.settle_until(tokio::time::Instant::now()).await;
        assert_eq!(drain.statuses()[0].1, StreamDrainStatus::TimedOut);
        invocation.cancel(ranvier_core::CancellationReason::Explicit);
        drain.settle_until(deadline).await;
        assert_eq!(drain.statuses()[0].1, StreamDrainStatus::Settled);
        let original = invocation
            .report()
            .take_report_until(deadline)
            .await
            .unwrap();
        assert!(matches!(
            original,
            crate::streaming_report_handle::SupervisedStreamCompletion::Report(_)
        ));
        drop(invocation);
        drop(supervisor);
        assert_eq!(admission.in_use(), Ok(1));
        drop(drain);
        assert_eq!(admission.in_use(), Ok(0));
    }
    #[tokio::test]
    async fn staged_preparation_drop_unpolled_start_abandon_and_closed_start_are_joinable() {
        #[derive(Clone)]
        struct CountPrefix(Arc<AtomicUsize>);
        #[async_trait::async_trait]
        impl ranvier_core::Transition<u32, u32> for CountPrefix {
            type Error = String;
            type Resources = ();
            async fn run(
                &self,
                input: u32,
                _: &(),
                _: &mut ranvier_core::Bus,
            ) -> ranvier_core::Outcome<u32, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                ranvier_core::Outcome::Next(input)
            }
        }
        for mode in 0..5 {
            let supervisor = StreamSupervisor::new(1);
            let admission = supervisor.admission.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let circuit = crate::Axon::<u32, u32, String>::start("staged-gate")
                .then(CountPrefix(calls.clone()))
                .then_stream(Finite);
            let (ticket, context) = supervisor
                .reserve()
                .unwrap()
                .with_context(|| {
                    OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())
                })
                .unwrap();
            let prepared = match ticket.prepare(
                &circuit,
                context,
                317,
                std::time::Duration::from_secs(1),
                None,
            ) {
                Ok(prepared) => prepared,
                Err(_) => panic!("setup rejected"),
            };
            let report = prepared.report().clone();
            let parent = ranvier_core::CancellationToken::new();
            let mut drain = supervisor.close_for_drain().unwrap();
            assert_eq!(drain.unfinalized_reservations(), 0);
            assert_eq!(drain.statuses().len(), 1);
            assert_eq!(drain.statuses()[0].1, StreamDrainStatus::Pending);
            let supervisor = if mode == 4 {
                drop(supervisor);
                None
            } else {
                Some(supervisor)
            };
            let rejected = match mode {
                0 => {
                    drop(prepared);
                    None
                }
                1 => {
                    drop(prepared.start(parent.clone()));
                    None
                }
                2 => Some(prepared.abandon()),
                _ => match prepared.start(parent.clone()).await {
                    Err(rejected) => Some(rejected),
                    Ok(_) => panic!("retired preparation started"),
                },
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            drain.settle_until(deadline).await;
            assert_eq!(drain.statuses()[0].1, StreamDrainStatus::Settled);
            assert!(matches!(
                report.take_report_until(deadline).await.unwrap(),
                crate::streaming_report_handle::SupervisedStreamCompletion::RejectedBeforeStart
            ));
            if let Some(rejected) = &rejected {
                rejected.retire_unstarted_until(deadline).await.unwrap();
            }
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(!parent.is_cancelled());
            drop(rejected);
            drop(report);
            drop(supervisor);
            assert_eq!(admission.in_use(), Ok(1));
            drop(drain);
            assert_eq!(admission.in_use(), Ok(0));
        }
    }
    #[test]
    fn paired_reservation_precedes_factory_and_refunds_failure() {
        let supervisor = StreamSupervisor::new(1);
        let calls = AtomicUsize::new(0);
        let ticket = supervisor.reserve().unwrap();
        assert!(matches!(
            supervisor.reserve(),
            Err(StreamReservationError::RecoveryCapacity)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let result = ticket.with_context::<(), _>(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("original factory error")
        });
        assert!(matches!(
            result,
            Err(StreamContextFactoryError::Factory("original factory error"))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
    }
    #[test]
    fn closed_or_released_root_does_not_call_context_factory() {
        for released in [false, true] {
            let supervisor = StreamSupervisor::new(1);
            let ticket = supervisor.reserve().unwrap();
            let admission = supervisor.admission.clone();
            if !released {
                supervisor.close().unwrap();
            }
            drop(supervisor);
            let calls = AtomicUsize::new(0);
            let result = ticket.with_context(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())
            });
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(admission.in_use(), Ok(0));
        }
    }
    #[test]
    fn partial_root_failure_and_factory_unwind_refund_recovery() {
        let supervisor = StreamSupervisor::new(1);
        let occupied = supervisor.root.reserve(Arc::new(227u32)).unwrap();
        assert!(matches!(
            supervisor.reserve(),
            Err(StreamReservationError::RootCapacity)
        ));
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        occupied.release();
        let ticket = supervisor.reserve().unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = ticket.with_context::<(), ()>(|| panic!("private callback panic"));
            }))
            .is_err()
        );
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
        supervisor.root.close_admission().unwrap();
        assert!(matches!(
            supervisor.reserve(),
            Err(StreamReservationError::RootClosed)
        ));
        assert_eq!(supervisor.admission.in_use(), Ok(0));
    }
    #[test]
    fn context_factory_error_debug_does_not_expose_private_values() {
        struct Secret;
        let error = StreamContextFactoryError::Factory(Secret);
        assert_eq!(format!("{error:?}"), "StreamContextFactoryError::Factory");
    }
    #[test]
    fn successful_ticket_cannot_allocate_a_second_context() {
        let supervisor = StreamSupervisor::new(1);
        let (ticket, original) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| {
                OwnedStreamContext::try_new(Arc::new(229u32), ranvier_core::Bus::new())
            })
            .unwrap();
        let calls = AtomicUsize::new(0);
        let result = ticket.with_context(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            OwnedStreamContext::try_new(Arc::new(()), ranvier_core::Bus::new())
        });
        assert!(matches!(
            result,
            Err(StreamContextFactoryError::AlreadyCreated)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert_eq!(*original.into_parts().0, 229);
    }
    #[test]
    fn successful_factory_retains_exact_context_and_unique_ticket() {
        let supervisor = StreamSupervisor::new(1);
        let resources = Arc::new(211u32);
        let (ticket, context) = supervisor
            .reserve()
            .unwrap()
            .with_context(|| {
                let mut bus = ranvier_core::Bus::new();
                bus.insert(223u32);
                OwnedStreamContext::try_new(resources.clone(), bus)
            })
            .unwrap();
        let (returned, bus) = context.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(*bus.read::<u32>().unwrap(), 223);
        assert_eq!(supervisor.admission.in_use(), Ok(1));
        assert!(supervisor.reserve().is_err());
        drop(ticket);
        assert_eq!(supervisor.admission.in_use(), Ok(0));
        assert!(supervisor.reserve().is_ok());
    }
}
