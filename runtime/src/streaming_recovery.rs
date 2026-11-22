//! Internal ownership primitive. Production admission must reserve bounded recovery
//! capacity before constructing this owner; this module does not spawn tasks.
use crate::streaming_recovery_admission::{
    RecoveryAdmissionFailure, RecoveryPermit, StreamRecoveryAdmission,
};
use crate::{Axon, StreamInitializationReport, streaming_axon::OwnedStreamContext};
use ranvier_core::{Bus, CancellationToken, streaming::StreamingTransition};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;

type RawStream<Item> = Pin<Box<dyn futures_core::Stream<Item = Item> + Send>>;

/// Bound on journal rows one recovery call may walk, and on nested depth.
const NESTED_RECOVERY_STEPS: usize = 256;
const NESTED_RECOVERY_DEPTH: usize = 32;

/// Local consumption observation, separate from stored prefix completion.
pub(crate) struct StreamDriveResult {
    pub(crate) terminal: crate::StreamConsumptionTerminal,
}
/// Fixed late failure classification; panic payload is never returned.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamDriverError {
    Panicked,
    Cancelled(ranvier_core::CancellationContext),
}
pub(crate) struct StreamReadyResult {
    pub(crate) consumption: Result<StreamDriveResult, StreamDriverError>,
    pub(crate) cleanup: Option<crate::SagaRollbackReport>,
    /// Durability of the stream end record; `None` when no journal was kept.
    pub(crate) end_durability: Option<crate::StreamDurability>,
}

/// Consume against the actual gated invocation origin rather than restarting total.
pub(crate) async fn produce_bounded_with_total_origin<Item: Send + 'static>(
    source: RawStream<Item>,
    config: ranvier_core::streaming::StreamTimeoutConfig,
    token: CancellationToken,
    sender: tokio::sync::mpsc::Sender<Item>,
    invocation_started_at: tokio::time::Instant,
) -> StreamDriveResult {
    use futures_util::{FutureExt, StreamExt};
    let mut observed = crate::ObservedStream::with_total_origin(
        source,
        config,
        Some(token),
        invocation_started_at,
    );
    let terminal = loop {
        let permit = tokio::select! {
            biased;
            terminal = observed.stopped() => break terminal,
            _ = sender.closed() => break crate::StreamConsumptionTerminal::Cancelled(
                ranvier_core::CancellationContext::new(ranvier_core::CancellationReason::ClientDisconnected)),
            permit = sender.reserve() => match permit {
                Ok(permit) => permit,
                Err(_) => break crate::StreamConsumptionTerminal::Cancelled(
                    ranvier_core::CancellationContext::new(ranvier_core::CancellationReason::ClientDisconnected)),
            },
        };
        let item = tokio::select! {
            biased;
            _ = sender.closed() => break crate::StreamConsumptionTerminal::Cancelled(
                ranvier_core::CancellationContext::new(ranvier_core::CancellationReason::ClientDisconnected)),
            item = observed.next() => item,
        };
        match item {
            Some(item) => {
                if let Some(terminal) = observed.stopped().now_or_never() {
                    break terminal;
                }
                permit.send(item);
            }
            None => break observed.terminal().cloned().expect("EOF records terminal"),
        }
    };
    drop(observed);
    StreamDriveResult { terminal }
}

/// Safe observation only: Returned means a local report exists, not durable success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SupervisorPhase {
    NotStarted,
    Initializing,
    Consuming,
    CleaningUp,
    Returned,
    Interrupted,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SupervisorDisposition {
    PrefixRejected,
    InitializationFailed,
    InitializationCancelled,
    Consumption(crate::StreamConsumptionTerminal),
    DriverPanicked,
}
/// Custody of this owner only; release does not prove effects reversed or secure erasure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryCustody {
    Held,
    ReleasedWithPendingTasks,
    /// Custody ended while retained state could not be established. No empty
    /// journal, cleanup completion or successful reconciliation is inferred.
    ReleasedWithUnknownState,
    ReleasedWithoutRetainedTasks,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SupervisorObservation {
    pub(crate) phase: SupervisorPhase,
    pub(crate) disposition: Option<SupervisorDisposition>,
    pub(crate) cleanup_complete: Option<bool>,
    pub(crate) cleanup_timed_out: bool,
    pub(crate) custody: RecoveryCustody,
}
struct SupervisorObservationGuard {
    sender: tokio::sync::watch::Sender<SupervisorObservation>,
    armed: bool,
}
impl SupervisorObservationGuard {
    fn phase(&self, phase: SupervisorPhase) {
        self.sender.send_modify(|state| state.phase = phase);
    }
    fn returned(mut self, disposition: SupervisorDisposition, cleanup_complete: Option<bool>) {
        self.sender.send_modify(|state| {
            state.phase = SupervisorPhase::Returned;
            state.disposition = Some(disposition);
            state.cleanup_complete = cleanup_complete;
        });
        self.armed = false;
    }
}
impl Drop for SupervisorObservationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.phase(SupervisorPhase::Interrupted);
        }
    }
}

#[derive(Clone)]
pub(crate) struct ParentStreamCustody(
    crate::nested_custody::WeakCustody,
    Option<CancellationToken>,
);

/// A single invocation's exact graph, typed resources and request Bus. An external
/// clone must outlive the supervisor, including forced task abort. No replay API.
pub(crate) struct OwnedStreamRecovery<In, Out, E, Res> {
    inner: Arc<RecoveryInner<In, Out, E, Res>>,
}
struct RecoveryInner<In, Out, E, Res> {
    graph: Axon<In, Out, E, Res>,
    context: Mutex<(Arc<Res>, Bus)>,
    started: AtomicBool,
    reconciliation: Mutex<()>,
    expires: Option<std::time::Instant>,
    observation: tokio::sync::watch::Sender<SupervisorObservation>,
    // Last field: release reservation after graph/context fields are dropped.
    _permit: RecoveryPermit,
}
/// Temporarily installs fresh cooperative cleanup control while removing the
/// transition access guard. The exact prior Bus scopes return on every exit.
struct LocalCompensationBusScope<'a> {
    bus: &'a mut Bus,
    access: Option<ranvier_core::bus::BusAccessScope>,
    previous_token: Option<CancellationToken>,
    operation_token: CancellationToken,
}
impl<'a> LocalCompensationBusScope<'a> {
    fn begin(bus: &'a mut Bus) -> Self {
        let access = bus.take_access_scope();
        let operation_token = CancellationToken::new();
        let previous_token = bus.replace_cancellation_token(Some(operation_token.clone()));
        Self {
            bus,
            access: Some(access),
            previous_token,
            operation_token,
        }
    }
    fn bus(&mut self) -> &mut Bus {
        self.bus
    }
}
impl Drop for LocalCompensationBusScope<'_> {
    fn drop(&mut self) {
        self.operation_token
            .cancel(ranvier_core::CancellationReason::Explicit);
        self.bus
            .replace_cancellation_token(self.previous_token.take());
        if let Some(access) = self.access.take() {
            self.bus.restore_access_scope(access);
        }
    }
}
impl<In, Out, E, Res> Drop for RecoveryInner<In, Out, E, Res> {
    fn drop(&mut self) {
        // The last Arc is gone: there can be no live task/context-lock borrower.
        let (_, bus) = self.context.get_mut();
        let access = bus.take_access_scope();
        let stack_pending = bus
            .read::<ranvier_core::saga::SagaStack>()
            .is_some_and(|stack| !stack.is_empty());
        let journal_pending = bus
            .read::<crate::SagaRollbackJournal>()
            .map(|journal| journal.try_retained_work_pending(256))
            .transpose();
        bus.restore_access_scope(access);
        self.observation.send_modify(|state| {
            state.custody = if stack_pending || matches!(journal_pending, Ok(Some(true))) {
                RecoveryCustody::ReleasedWithPendingTasks
            } else if journal_pending.is_err() {
                RecoveryCustody::ReleasedWithUnknownState
            } else {
                RecoveryCustody::ReleasedWithoutRetainedTasks
            }
        });
        // Preserve local phase/disposition/cleanup evidence. This is custody release,
        // not a new success and not automatic compensation/replay.
    }
}
impl<In, Out, E, Res> Clone for OwnedStreamRecovery<In, Out, E, Res> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
/// Failed pre-effect admission retains the original graph and request context.
pub(crate) struct StreamRecoveryAdmissionRejected<In, Out, E, Res> {
    graph: Axon<In, Out, E, Res>,
    context: OwnedStreamContext<Res>,
    reason: RecoveryAdmissionFailure,
}
impl<In, Out, E, Res> std::fmt::Debug for StreamRecoveryAdmissionRejected<In, Out, E, Res> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamRecoveryAdmissionRejected")
            .field("reason", &self.reason)
            .finish()
    }
}
impl<In, Out, E, Res> StreamRecoveryAdmissionRejected<In, Out, E, Res> {
    pub(crate) fn into_parts(self) -> (Axon<In, Out, E, Res>, OwnedStreamContext<Res>) {
        (self.graph, self.context)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StreamInvocationAlreadyStarted;
/// Failure retains any successfully registered custody for explicit recovery.
pub(crate) struct ManagedStreamStartError {
    pub(crate) signal: &'static str,
    pub(crate) custody: Option<crate::nested_custody::WeakCustody>,
}
impl<In, Out, E, Res> OwnedStreamRecovery<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: Send + Sync + 'static,
{
    pub(crate) fn try_new(
        graph: Axon<In, Out, E, Res>,
        context: OwnedStreamContext<Res>,
        admission: &StreamRecoveryAdmission,
        expires: Option<std::time::Instant>,
    ) -> Result<Self, StreamRecoveryAdmissionRejected<In, Out, E, Res>> {
        let permit = match admission.reserve() {
            Ok(permit) => permit,
            Err(reason) => {
                return Err(StreamRecoveryAdmissionRejected {
                    graph,
                    context,
                    reason,
                });
            }
        };
        Self::try_new_reserved(graph, context, admission, expires, permit)
    }
    /// Consume one exact pre-admitted permit, without allocating another slot.
    pub(crate) fn try_new_reserved(
        mut graph: Axon<In, Out, E, Res>,
        context: OwnedStreamContext<Res>,
        admission: &StreamRecoveryAdmission,
        expires: Option<std::time::Instant>,
        permit: RecoveryPermit,
    ) -> Result<Self, StreamRecoveryAdmissionRejected<In, Out, E, Res>> {
        if let Err(reason) = admission.validate_permit(&permit) {
            return Err(StreamRecoveryAdmissionRejected {
                graph,
                context,
                reason,
            });
        }
        // An owned recovery owner freezes the policy selection beside its
        // handler registry. A later Bus mutation or hot reload must not grant
        // compensation authority to an invocation that started with it disabled.
        if let Some(dynamic_policy) = graph.dynamic_saga_policy.take() {
            graph.saga_policy = dynamic_policy.current();
        }
        // Copy handler Arcs under the original read lock before any effects.
        // A poisoned registry has unknown authority: retain caller ownership.
        let handlers = graph
            .saga_compensation_registry
            .try_read()
            .map(|registry| registry.handlers.clone())
            .map_err(|error| match error {
                std::sync::TryLockError::WouldBlock => RecoveryAdmissionFailure::RegistryBusy,
                std::sync::TryLockError::Poisoned(_) => RecoveryAdmissionFailure::RegistryPoisoned,
            });
        let handlers = match handlers {
            Ok(handlers) => handlers,
            Err(reason) => {
                return Err(StreamRecoveryAdmissionRejected {
                    graph,
                    context,
                    reason,
                });
            }
        };
        graph.saga_compensation_registry = Arc::new(std::sync::RwLock::new(
            ranvier_core::saga::SagaCompensationRegistry { handlers },
        ));
        Ok(Self {
            inner: Arc::new(RecoveryInner {
                graph,
                context: Mutex::new(context.into_parts()),
                started: AtomicBool::new(false),
                reconciliation: Mutex::new(()),
                expires,
                _permit: permit,
                observation: tokio::sync::watch::channel(SupervisorObservation {
                    phase: SupervisorPhase::NotStarted,
                    disposition: None,
                    cleanup_complete: None,
                    cleanup_timed_out: false,
                    custody: RecoveryCustody::Held,
                })
                .0,
            }),
        })
    }
    /// Independent of item queue/context lock; contains no request/input payloads.
    pub(crate) fn observe(&self) -> tokio::sync::watch::Receiver<SupervisorObservation> {
        self.inner.observation.subscribe()
    }

    /// Publish the exact unopened frame before preparation can be abandoned.
    pub(crate) fn stage_reserved<T: Send + 'static>(
        &self,
        client: &crate::nested_custody::WeakCustodyClient,
        slot: &mut crate::nested_custody::CustodyReservation,
        journal: &crate::SagaRollbackJournal,
        prepared: &mut crate::managed_stream_task::PreparedStreamTask<T>,
    ) -> Result<crate::nested_custody::WeakCustody, ManagedStreamStartError> {
        if !prepared.matches_owner(&self.retention_capability()) || !prepared.has_open_start() {
            let _ = prepared.reject_start();
            return Err(ManagedStreamStartError {
                signal: "execution.nested_task_owner_mismatch",
                custody: None,
            });
        }
        let custody = match slot.finalize_managed(
            client,
            self.inner.clone(),
            prepared,
            journal,
            self.observe(),
        ) {
            Ok(custody) => custody,
            Err(signal) => {
                let _ = prepared.reject_start();
                return Err(ManagedStreamStartError {
                    signal,
                    custody: None,
                });
            }
        };
        Ok(custody)
    }
    pub(crate) fn start_staged<T: Send + 'static>(
        &self,
        client: crate::nested_custody::WeakCustodyClient,
        custody: &crate::nested_custody::WeakCustody,
        parent: CancellationToken,
        prepared: &mut crate::managed_stream_task::PreparedStreamTask<T>,
    ) -> Result<(), &'static str> {
        let result = (|| {
            if !prepared.has_open_start() || !prepared.matches_owner(&self.retention_capability()) {
                return Err("execution.nested_task_start_decided");
            }
            let mut context = self
                .inner
                .context
                .try_lock()
                .map_err(|_| "execution.nested_stream_context_busy")?;
            if self.inner.started.load(Ordering::Acquire) {
                return Err("execution.nested_stream_already_started");
            }
            let bus = &mut context.1;
            let access = bus.take_access_scope();
            let bound = (|| {
                if bus.has::<ParentStreamCustody>()
                    || bus.has::<crate::nested_custody::WeakCustodyClient>()
                    || bus.has::<crate::nested_custody::CustodyStore>()
                {
                    return Err("execution.nested_stream_already_bound");
                }
                client.validate_staged_task(
                    custody,
                    &self.retention_capability(),
                    &(prepared.holder() as crate::nested_custody::Capability),
                )?;
                bus.insert(client);
                bus.insert(ParentStreamCustody(
                    custody.clone(),
                    Some(parent.child_token()),
                ));
                prepared.grant_registered_start(custody)
            })();
            bus.restore_access_scope(access);
            bound
        })();
        if result.is_err() {
            let _ = prepared.reject_start();
        }
        result
    }

    pub(crate) fn recovery_probe(&self) -> crate::streaming_report_handle::RecoveryProbe {
        let owner = self.clone();
        Arc::new(move |deadline| {
            let owner = owner.clone();
            Box::pin(async move {
                use crate::streaming_report_handle::{
                    StreamLiveRecoveryState, StreamRecoveryProbeError as Error,
                };
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::TimedOut);
                }
                let mut context = tokio::time::timeout_at(deadline, owner.inner.context.lock())
                    .await
                    .map_err(|_| Error::TimedOut)?;
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::TimedOut);
                }
                let bus = &mut context.1;
                let access = bus.take_access_scope();
                let result = (|| {
                    let saga_stack_nonempty = bus
                        .read::<ranvier_core::saga::SagaStack>()
                        .is_some_and(|stack| !stack.is_empty());
                    let journal = bus.read::<crate::SagaRollbackJournal>();
                    let recovery_pending = match journal {
                        Some(journal) => journal.try_recovery_pending(256)?,
                        None => false,
                    };
                    Ok(StreamLiveRecoveryState {
                        saga_stack_nonempty,
                        journal_present: journal.is_some(),
                        recovery_pending,
                    })
                })();
                bus.restore_access_scope(access);
                result
            })
        })
    }
    /// Bind explicit local compensation to the exact original joined report and
    /// typed owner. This closure cannot be constructed from a copied assessment.
    pub(crate) fn local_recovery(
        &self,
        report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    ) -> crate::streaming_report_handle::LocalRecovery {
        let owner = self.clone();
        Arc::new(move |deadline| {
            let owner = owner.clone();
            let report = report.clone();
            Box::pin(async move {
                report.require_reported_frame(deadline).await?;
                owner
                    .reconcile_next_local_compensation_until(deadline)
                    .await
            })
        })
    }
    /// Bind marker-only external compensation reconciliation to this report's
    /// exact original owner. The retained hook is never invoked by this path.
    pub(crate) fn external_recovery(
        &self,
        report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    ) -> crate::streaming_report_handle::ExternalRecovery {
        let owner = self.clone();
        Arc::new(move |deadline| {
            let owner = owner.clone();
            let report = report.clone();
            Box::pin(async move {
                report.require_reported_frame(deadline).await.map_err(|error| match error {
                    crate::streaming_report_handle::StreamLocalRecoveryError::Wait(error) =>
                        crate::streaming_report_handle::StreamExternalRecoveryError::Wait(error),
                    crate::streaming_report_handle::StreamLocalRecoveryError::FrameNotReported =>
                        crate::streaming_report_handle::StreamExternalRecoveryError::FrameNotReported,
                    _ => crate::streaming_report_handle::StreamExternalRecoveryError::Unavailable,
                })?;
                owner
                    .readback_next_external_compensation_until(deadline)
                    .await
            })
        })
    }
    async fn reconcile_next_local_compensation_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<crate::SagaRollbackOutcome, crate::streaming_report_handle::StreamLocalRecoveryError>
    {
        use crate::saga_report::LocalTaskClaimError;
        use crate::streaming_report_handle::{
            StreamLocalRecoveryError as Error, StreamReportWaitError,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Wait(StreamReportWaitError::TimedOut));
        }
        let _operation = tokio::time::timeout_at(deadline, self.inner.reconciliation.lock())
            .await
            .map_err(|_| Error::Wait(StreamReportWaitError::TimedOut))?;
        if !self.inner.started.load(Ordering::Acquire) {
            return Err(Error::FrameNotReported);
        }
        let mut context = tokio::time::timeout_at(deadline, self.inner.context.lock())
            .await
            .map_err(|_| Error::Wait(StreamReportWaitError::TimedOut))?;
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Wait(StreamReportWaitError::TimedOut));
        }
        let (resources, bus) = &mut *context;
        let mut scope = LocalCompensationBusScope::begin(bus);
        let bus = scope.bus();
        if self.inner.graph.saga_policy != ranvier_core::saga::SagaPolicy::Enabled
            || !bus
                .read::<ranvier_core::saga::SagaPolicy>()
                .is_some_and(|policy| *policy == ranvier_core::saga::SagaPolicy::Enabled)
        {
            return Err(Error::RecoveryBlocked);
        }
        let journal = if let Some(journal) = bus.read::<crate::SagaRollbackJournal>().cloned() {
            journal
        } else if bus
            .read::<ranvier_core::saga::SagaStack>()
            .is_some_and(|stack| !stack.is_empty())
        {
            let trace = bus.id.to_string();
            let journal = crate::SagaRollbackJournal::new(&trace);
            bus.insert(journal.clone());
            journal
        } else {
            return Err(Error::JournalUnavailable);
        };
        // Preserve LIFO by moving every unprocessed stack item into the same
        // indexed journal. Each task moves once; no serialized input is cloned.
        if let Some(stack) = bus.read_mut::<ranvier_core::saga::SagaStack>() {
            while let Some(task) = stack.pop() {
                match journal.try_push_uninvoked_task(task) {
                    Ok(_) => {}
                    Err((_, task)) => {
                        stack.tasks.push(task);
                        return Err(Error::Unavailable);
                    }
                }
            }
        }
        // Walk retained nested journals in LIFO order. A nested row is undone
        // with the child's own resources and frozen handlers from custody.
        let map = |error: LocalTaskClaimError| match error {
            LocalTaskClaimError::Unavailable => Error::Unavailable,
            LocalTaskClaimError::TimedOut => Error::Wait(StreamReportWaitError::TimedOut),
            LocalTaskClaimError::Missing
            | LocalTaskClaimError::NotEligible
            | LocalTaskClaimError::Claimed
            | LocalTaskClaimError::IdentityMismatch => Error::RecoveryBlocked,
        };
        let mut current = journal;
        let mut parents: Vec<(crate::SagaRollbackJournal, usize)> = Vec::new();
        let mut nested: Option<std::sync::Arc<dyn crate::nested_custody::NestedCompensation>> =
            None;
        for _ in 0..NESTED_RECOVERY_STEPS {
            match current.next_recovery_row().map_err(map)? {
                None | Some(crate::saga_report::RecoveryRow::Blocked) => {
                    return Err(Error::RecoveryBlocked);
                }
                Some(crate::saga_report::RecoveryRow::Child { index, journal }) => {
                    if current.settle_completed_child(index).map_err(map)? {
                        continue;
                    }
                    if parents.len() >= NESTED_RECOVERY_DEPTH {
                        return Err(Error::RecoveryBlocked);
                    }
                    let record = journal
                        .retained_custody()
                        .and_then(|record| {
                            record
                                .downcast::<crate::nested_custody::NestedCustodyRecord>()
                                .ok()
                        })
                        .ok_or(Error::RecoveryBlocked)?;
                    nested = Some(record.0.clone());
                    parents.push((std::mem::replace(&mut current, journal), index));
                }
                Some(crate::saga_report::RecoveryRow::Task) => {
                    let mut claim = current.claim_next_not_invoked().map_err(map)?;
                    let outcome = match &nested {
                        Some(child) => child.compensate(claim, scope.bus(), deadline).await,
                        None => {
                            let handler = self
                                .inner
                                .graph
                                .saga_compensation_registry
                                .try_read()
                                .map_err(|_| Error::HandlerRegistryUnavailable)?
                                .get(claim.node_id());
                            match handler {
                                Some(handler) => {
                                    claim
                                        .invoke(handler, resources.as_ref(), scope.bus(), deadline)
                                        .await
                                }
                                None => claim
                                    .mark_missing_handler()
                                    .map(|()| crate::SagaRollbackOutcome::MissingHandler),
                            }
                        }
                    }
                    .map_err(map)?;
                    // Completing the last child row lets its parent row settle too.
                    while let Some((parent, index)) = parents.pop() {
                        if !parent.settle_completed_child(index).map_err(map)? {
                            break;
                        }
                    }
                    return Ok(outcome);
                }
            }
        }
        Err(Error::RecoveryBlocked)
    }
    async fn readback_next_external_compensation_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<
        crate::streaming_report_handle::StreamExternalReadback,
        crate::streaming_report_handle::StreamExternalRecoveryError,
    > {
        use crate::saga_report::ExternalTaskClaimError;
        use crate::streaming_report_handle::{
            StreamExternalRecoveryError as Error, StreamReportWaitError,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Wait(StreamReportWaitError::TimedOut));
        }
        let _operation = tokio::time::timeout_at(deadline, self.inner.reconciliation.lock())
            .await
            .map_err(|_| Error::Wait(StreamReportWaitError::TimedOut))?;
        if !self.inner.started.load(Ordering::Acquire) {
            return Err(Error::FrameNotReported);
        }
        let context = tokio::time::timeout_at(deadline, self.inner.context.lock())
            .await
            .map_err(|_| Error::Wait(StreamReportWaitError::TimedOut))?;
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Wait(StreamReportWaitError::TimedOut));
        }
        let journal = context
            .1
            .read::<crate::SagaRollbackJournal>()
            .cloned()
            .ok_or(Error::JournalUnavailable)?;
        // The journal claim retains the exact capability. Marker I/O does not
        // need the mutable owner Bus/resources context, so release it before
        // awaiting a user-provided store while keeping the recovery operation
        // serialized by `_operation`.
        drop(context);
        let claim = journal.claim_next_external().map_err(|error| match error {
            ExternalTaskClaimError::Unavailable => Error::Unavailable,
            ExternalTaskClaimError::TimedOut => Error::Wait(StreamReportWaitError::TimedOut),
            ExternalTaskClaimError::Missing
            | ExternalTaskClaimError::NotEligible
            | ExternalTaskClaimError::Claimed
            | ExternalTaskClaimError::IdentityMismatch => Error::RecoveryBlocked,
        })?;
        claim
            .readback_until(deadline)
            .await
            .map_err(|error| match error {
                ExternalTaskClaimError::Unavailable => Error::Unavailable,
                ExternalTaskClaimError::TimedOut => Error::Wait(StreamReportWaitError::TimedOut),
                ExternalTaskClaimError::Missing
                | ExternalTaskClaimError::NotEligible
                | ExternalTaskClaimError::Claimed
                | ExternalTaskClaimError::IdentityMismatch => Error::RecoveryBlocked,
            })
    }
    /// Constructed only beside the exact staged report and receipt. The captured
    /// report has no retirement callback, so this lease cannot form a cycle.
    pub(crate) fn local_retirement(
        &self,
        receipt: crate::nested_custody::WeakCustody,
        report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    ) -> crate::streaming_report_handle::LocalRetirement {
        self.retirement(receipt, report, false)
    }
    /// Release for `StreamReportHandle::release_settled_until`: a normally
    /// completed stream discards its prefix Saga stack, as an ordinary Axon
    /// does on success; otherwise only settled, fully reconciled state goes.
    pub(crate) fn completed_finalization(
        &self,
        receipt: crate::nested_custody::WeakCustody,
        report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
    ) -> crate::streaming_report_handle::LocalRetirement {
        self.retirement(receipt, report, true)
    }
    fn retirement(
        &self,
        receipt: crate::nested_custody::WeakCustody,
        report: crate::streaming_report_handle::StreamReportHandle<Out, E>,
        completed_stream: bool,
    ) -> crate::streaming_report_handle::LocalRetirement {
        let owner = self.clone();
        let decision = Arc::new(Mutex::new(false));
        Arc::new(move |deadline| {
            let owner = owner.clone();
            let receipt = receipt.clone();
            let report = report.clone();
            let decision = decision.clone();
            Box::pin(async move {
                use crate::streaming_report_handle::{
                    StreamLocalRetirementError as Error, StreamReportWaitError,
                };
                let timeout = || Error::Wait(StreamReportWaitError::TimedOut);
                if tokio::time::Instant::now() >= deadline {
                    return Err(timeout());
                }
                let mut retired = tokio::time::timeout_at(deadline, decision.lock())
                    .await
                    .map_err(|_| timeout())?;
                if tokio::time::Instant::now() >= deadline {
                    return Err(timeout());
                }
                if *retired {
                    return Ok(());
                }
                let settlement = if completed_stream {
                    Some(report.prove_settled(deadline).await?)
                } else {
                    report.prove_completed_local(deadline).await?;
                    None
                };
                let mut context = tokio::time::timeout_at(deadline, owner.inner.context.lock())
                    .await
                    .map_err(|_| timeout())?;
                if tokio::time::Instant::now() >= deadline {
                    return Err(timeout());
                }
                let bus = &mut context.1;
                let access = bus.take_access_scope();
                // A clear snapshot of a clonable journal is not sealed authority.
                // Refuse every journal until child mutation/recovery is sealed.
                use crate::streaming_report_handle::StreamSettlement;
                let journal = bus.read::<crate::SagaRollbackJournal>().cloned();
                let stack = bus
                    .read::<ranvier_core::saga::SagaStack>()
                    .is_some_and(|stack| !stack.is_empty());
                // Settled release accepts a journal only after every row was
                // reconciled; the frame has joined and the context lock is held.
                let journal_blocks = match (&journal, settlement) {
                    (None, Some(StreamSettlement::RequiresReconciledJournal)) => true,
                    (None, _) => false,
                    (Some(journal), Some(_)) => !journal.is_fully_reconciled(),
                    (Some(_), None) => true,
                };
                let stack_blocks = stack && settlement != Some(StreamSettlement::Completed);
                if journal_blocks || stack_blocks {
                    bus.restore_access_scope(access);
                    return Err(Error::RecoveryReviewRequired);
                }
                // A normally completed stream needs no compensation: drop the
                // prefix inputs, as an ordinary successful Axon does.
                let discarded = if stack {
                    bus.remove::<ranvier_core::saga::SagaStack>()
                } else {
                    None
                };
                bus.restore_access_scope(access);
                drop(discarded);
                // No await between the guarded live check, exact removal and
                // cached success. Dropping a waiter cannot lose the decision.
                receipt
                    .release_exact_task(&owner.retention_capability(), &report.holder_capability())
                    .map_err(|signal| match signal {
                        "execution.parent_custody_released" => Error::CustodyReleased,
                        "execution.nested_task_registration_mismatch" => Error::CustodyMismatch,
                        _ => Error::CustodyUnavailable,
                    })?;
                *retired = true;
                Ok(())
            })
        })
    }
    /// Retain the exact graph/context owner through public report-handle waits.
    pub(crate) fn retention_capability(&self) -> crate::nested_custody::Capability {
        self.inner.clone()
    }

    /// Build the gated future from this exact recovery owner and owned inputs.
    /// The supervisor must register this owner's holder before granting start.
    pub(crate) fn prepare_managed_controlled<Item, SErr, S, F, Fut>(
        &self,
        input: In,
        streaming: S,
        cleanup_grace: std::time::Duration,
        total: Option<std::time::Duration>,
        drive: F,
    ) -> crate::managed_stream_task::PreparedStreamTask<
        Result<
            StreamInitializationReport<Out, StreamReadyResult, E>,
            StreamInvocationAlreadyStarted,
        >,
    >
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>,
        F: FnOnce(RawStream<Item>, CancellationToken, tokio::time::Instant) -> Fut + Send + 'static,
        Fut: Future<Output = StreamDriveResult> + Send + 'static,
    {
        let child = self.clone();
        crate::managed_stream_task::PreparedStreamTask::new_owned(self.inner.clone(), async move {
            child
                .initialize_and_drive_controlled(
                    input,
                    &streaming,
                    None,
                    cleanup_grace,
                    total,
                    drive,
                )
                .await
        })
    }

    pub(crate) async fn initialize_and_drive_controlled<Item, SErr, S, F, Fut>(
        &self,
        input: In,
        streaming: &S,
        token: Option<CancellationToken>,
        cleanup_grace: std::time::Duration,
        total: Option<std::time::Duration>,
        drive: F,
    ) -> Result<StreamInitializationReport<Out, StreamReadyResult, E>, StreamInvocationAlreadyStarted>
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>,
        F: FnOnce(RawStream<Item>, CancellationToken, tokio::time::Instant) -> Fut,
        Fut: Future<Output = StreamDriveResult>,
    {
        // Start only when the admitted gated future is actually polled.
        let invocation_started_at = tokio::time::Instant::now();
        if self
            .inner
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(StreamInvocationAlreadyStarted);
        }
        let observation = SupervisorObservationGuard {
            sender: self.inner.observation.clone(),
            armed: true,
        };
        observation.phase(SupervisorPhase::Initializing);
        let mut context = self.inner.context.lock().await;
        let (resources, bus) = &mut *context;
        let mut owner = crate::execution_observation::InvocationOwner::begin(bus);
        let parent_admission = owner.bus().read::<ParentStreamCustody>().cloned();
        let parent_client = owner
            .bus()
            .read::<crate::nested_custody::WeakCustodyClient>()
            .cloned();
        let inherited = parent_admission
            .as_ref()
            .and_then(|admission| admission.1.clone());
        let validation = if inherited.is_some() && token.is_some() {
            Err("execution.nested_stream_control_override")
        } else {
            match (parent_admission, parent_client) {
                (Some(admission), Some(client)) => client.validate_member(
                    &admission.0,
                    &(self.inner.clone() as Arc<dyn std::any::Any + Send + Sync>),
                ),
                (None, None) => Ok(()),
                _ => Err("execution.nested_stream_custody_mismatch"),
            }
        };
        if let Err(signal) = validation {
            let prefix = owner.finish(crate::ExecutionTerminal::Outcome(
                ranvier_core::Outcome::emit(signal, None),
            ));
            observation.returned(SupervisorDisposition::PrefixRejected, None);
            return Ok(StreamInitializationReport::Rejected { prefix });
        }
        let parent = inherited.or(token).unwrap_or_default();
        let token = match self.inner.expires {
            Some(expires) => parent
                .child_with_deadline(expires.saturating_duration_since(std::time::Instant::now())),
            None => parent,
        };
        // Consumption observes this same total origin. Only initialization needs
        // a child deadline token; do not leak its local control into the parent.
        let initialization_token = match total {
            Some(duration) => token.child_with_deadline_at(
                invocation_started_at
                    .checked_add(duration)
                    .unwrap_or(invocation_started_at),
            ),
            None => token.clone(),
        };
        owner
            .bus()
            .insert(crate::nested_custody::RequiredOwnedCustody);
        if !owner
            .bus()
            .has::<crate::nested_custody::WeakCustodyClient>()
        {
            owner
                .bus()
                .insert(crate::nested_custody::CustodyStore::new(128));
        }
        owner
            .bus()
            .insert(crate::nested_custody::OwnedResources(resources.clone()));
        if let Some(active) = owner
            .bus()
            .read::<crate::execution_observation::ActiveObservation>()
        {
            active.0.configure_cleanup(cleanup_grace);
            active.0.observe_cleanup(self.inner.observation.clone());
        }
        let report = self
            .inner
            .graph
            .initialize_stream_in_scope_with_cleanup_grace(
                input,
                resources.as_ref(),
                &mut owner,
                streaming,
                Some(initialization_token),
                Some(cleanup_grace),
                None,
            )
            .await;
        let mapped = match report {
            StreamInitializationReport::Ready { prefix, value } => {
                let crate::streaming_journal::InitializedStream {
                    source: value,
                    journal,
                } = value;
                observation.phase(SupervisorPhase::Consuming);
                let saga_enabled = owner
                    .bus()
                    .read::<ranvier_core::saga::SagaPolicy>()
                    .is_some_and(|policy| *policy == ranvier_core::saga::SagaPolicy::Enabled);
                use futures_util::FutureExt as _;
                // Count items pulled from the source for an abandoned-stream record;
                // the driver may be dropped mid-stream, so the count lives outside it.
                let observed_items = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let value: RawStream<_> = {
                    let counter = observed_items.clone();
                    Box::pin(futures_util::StreamExt::inspect(value, move |_| {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }))
                };
                // User factory invocation is inside the boundary, as well as polling.
                let consumption = std::panic::AssertUnwindSafe(async {
                    tokio::select! {
                        biased;
                        context = token.cancelled() => Err(StreamDriverError::Cancelled(context)),
                        observed = async { drive(value, token.clone(), invocation_started_at).await } => Ok(observed),
                    }
                })
                .catch_unwind()
                .await
                .unwrap_or(Err(StreamDriverError::Panicked));
                let cleanup = if !matches!(&consumption, Ok(observed) if observed.terminal == crate::StreamConsumptionTerminal::Completed)
                {
                    observation.phase(SupervisorPhase::CleaningUp);
                    self.inner
                        .graph
                        .cleanup_stream_initialization_with_grace(
                            resources.as_ref(),
                            owner.bus(),
                            saga_enabled,
                            Some(cleanup_grace),
                        )
                        .await
                } else {
                    // Source exhaustion is not an effect acknowledgment. Keep pending tasks.
                    None
                };
                // Record the stream effect's end after cleanup, as the public
                // consumer does before EOF. A client disconnect is recorded as an
                // abandoned stream with the number of items pulled from the source.
                let mut end_durability = None;
                if let Some(journal) = journal {
                    use crate::streaming_journal::StreamEnd;
                    let disconnected = |context: &ranvier_core::CancellationContext| {
                        context.reason == ranvier_core::CancellationReason::ClientDisconnected
                    };
                    let end = match &consumption {
                        Ok(observed) => match &observed.terminal {
                            crate::StreamConsumptionTerminal::Cancelled(context)
                                if disconnected(context) =>
                            {
                                StreamEnd::Abandoned
                            }
                            terminal => StreamEnd::from_terminal(terminal),
                        },
                        Err(StreamDriverError::Cancelled(context)) if disconnected(context) => {
                            StreamEnd::Abandoned
                        }
                        Err(StreamDriverError::Cancelled(_)) => StreamEnd::Cancelled,
                        Err(StreamDriverError::Panicked) => StreamEnd::DriverPanicked,
                    };
                    let items = (end == StreamEnd::Abandoned)
                        .then(|| observed_items.load(Ordering::Relaxed));
                    end_durability = Some(journal.finish(end, items).await);
                }
                StreamInitializationReport::Ready {
                    prefix,
                    value: StreamReadyResult {
                        consumption,
                        cleanup,
                        end_durability,
                    },
                }
            }
            StreamInitializationReport::Rejected { prefix } => {
                StreamInitializationReport::Rejected { prefix }
            }
            StreamInitializationReport::Failed {
                prefix,
                error,
                initialization,
                initialization_cleanup,
            } => StreamInitializationReport::Failed {
                prefix,
                error,
                initialization,
                initialization_cleanup,
            },
            StreamInitializationReport::Cancelled {
                prefix,
                context,
                initialization,
                initialization_cleanup,
            } => StreamInitializationReport::Cancelled {
                prefix,
                context,
                initialization,
                initialization_cleanup,
            },
        };
        let (disposition, cleanup_complete) = match &mapped {
            StreamInitializationReport::Ready { value, .. } => (
                match &value.consumption {
                    Ok(observed) => SupervisorDisposition::Consumption(observed.terminal.clone()),
                    Err(StreamDriverError::Panicked) => SupervisorDisposition::DriverPanicked,
                    Err(StreamDriverError::Cancelled(context)) => {
                        SupervisorDisposition::Consumption(
                            crate::StreamConsumptionTerminal::Cancelled(context.clone()),
                        )
                    }
                },
                value
                    .cleanup
                    .as_ref()
                    .map(crate::SagaRollbackReport::is_complete),
            ),
            StreamInitializationReport::Rejected { prefix } => (
                SupervisorDisposition::PrefixRejected,
                prefix.cleanup().map(crate::SagaRollbackReport::is_complete),
            ),
            StreamInitializationReport::Failed {
                initialization_cleanup,
                ..
            } => (
                SupervisorDisposition::InitializationFailed,
                initialization_cleanup
                    .as_ref()
                    .map(crate::SagaRollbackReport::is_complete),
            ),
            StreamInitializationReport::Cancelled {
                initialization_cleanup,
                ..
            } => (
                SupervisorDisposition::InitializationCancelled,
                initialization_cleanup
                    .as_ref()
                    .map(crate::SagaRollbackReport::is_complete),
            ),
        };
        drop(owner);
        drop(context);
        observation.returned(disposition, cleanup_complete);
        Ok(mapped)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::saga_report::tests::JournalTestExt as _;
    use crate::streaming_recovery_admission::tests::AdmissionTestExt as _;
    /// Test-only access to a rejected admission's reason.
    pub(crate) trait AdmissionRejectedTestExt {
        fn reason(&self) -> RecoveryAdmissionFailure;
    }
    impl<In, Out, E, Res> AdmissionRejectedTestExt
        for StreamRecoveryAdmissionRejected<In, Out, E, Res>
    {
        fn reason(&self) -> RecoveryAdmissionFailure {
            self.reason
        }
    }
    use ranvier_core::{
        Outcome, Transition,
        saga::{SagaPolicy, SagaStack},
    };
    use std::sync::atomic::AtomicUsize;
    #[test]
    fn local_compensation_bus_scope_restores_the_original_access_and_control_state() {
        let mut bus = Bus::new();
        bus.insert(SagaStack::new());
        bus.insert(41u32);
        let original = CancellationToken::new();
        bus.set_cancellation_token(original.clone());
        bus.set_access_policy(
            "original-scope",
            Some(ranvier_core::BusAccessPolicy::allow_only(vec![
                ranvier_core::BusTypeRef::of::<u32>(),
            ])),
        );
        assert!(bus.get::<SagaStack>().is_err());
        let operation = {
            let mut scope = LocalCompensationBusScope::begin(&mut bus);
            let operation = scope.bus().cancellation_token().cloned().unwrap();
            assert!(scope.bus().get::<SagaStack>().is_ok());
            drop(scope);
            operation
        };
        assert!(operation.is_cancelled());
        assert!(bus.get::<SagaStack>().is_err());
        assert_eq!(*bus.get::<u32>().unwrap(), 41);
        original.cancel(ranvier_core::CancellationReason::ClientDisconnected);
        assert!(
            bus.cancellation_token()
                .is_some_and(CancellationToken::is_cancelled)
        );
    }
    #[tokio::test]
    async fn required_nested_custody_rejects_mismatch_capacity_and_busy_registry_before_effects() {
        for mode in 0..3 {
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let graph = Axon::<u32, u32, String, Resources>::start("custody-pre-effect")
                .then(FailingEffect);
            let mut bus = Bus::new();
            bus.insert(crate::nested_custody::RequiredOwnedCustody);
            let configured = if mode == 0 {
                Arc::new(Resources {
                    effects: AtomicUsize::new(0),
                })
            } else {
                resources.clone()
            };
            bus.insert(crate::nested_custody::OwnedResources(configured));
            bus.insert(crate::nested_custody::CustodyStore::new(if mode == 1 {
                0
            } else {
                1
            }));
            bus.set_access_policy(
                "deny-framework-fixture",
                Some(ranvier_core::BusAccessPolicy::allow_only(Vec::new())),
            );
            let held = if mode == 2 {
                Some(graph.saga_compensation_registry.write().unwrap())
            } else {
                None
            };
            let report = graph.execute(181, resources.as_ref(), &mut bus).await;
            assert!(
                matches!(report, crate::ExecutionReport::Unpersisted { result: Outcome::Emit(signal, _), .. }
                if signal == match mode { 0 => "execution.nested_resources_unowned", 1 => "execution.nested_custody_capacity", _ => "execution.nested_registry_unavailable" })
            );
            assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
            drop(held);
        }
    }
    struct DistinctChildResources {
        effects: AtomicUsize,
    }
    #[derive(Clone)]
    struct DistinctChildEffect;
    #[async_trait::async_trait]
    impl Transition<u32, u32> for DistinctChildEffect {
        type Error = String;
        type Resources = DistinctChildResources;
        async fn run(
            &self,
            input: u32,
            resources: &DistinctChildResources,
            _: &mut Bus,
        ) -> Outcome<u32, String> {
            resources.effects.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(input)
        }
    }
    #[tokio::test]
    async fn explicit_owned_child_without_supervisor_rejects_before_effects_and_restores_owner() {
        for cancellable in [false, true] {
            let parent = Arc::new(DistinctChildResources {
                effects: AtomicUsize::new(0),
            });
            let child = Arc::new(DistinctChildResources {
                effects: AtomicUsize::new(0),
            });
            let mut bus = Bus::new();
            bus.insert(crate::nested_custody::OwnedResources(parent.clone()));
            let graph = Axon::<u32, u32, String, DistinctChildResources>::start("no-supervisor")
                .then(DistinctChildEffect);
            if cancellable {
                assert!(
                    matches!(graph.execute_owned_child_cancellable(201, child.clone(), &mut bus, CancellationToken::new()).await,
                    crate::ExecutionReport::Unpersisted { result: crate::ExecutionTerminal::Outcome(Outcome::Emit(signal, _)), .. }
                        if signal == "execution.owned_child_requires_supervisor")
                );
            } else {
                assert!(
                    matches!(graph.execute_owned_child(201, child.clone(), &mut bus).await,
                    crate::ExecutionReport::Unpersisted { result: Outcome::Emit(signal, _), .. }
                        if signal == "execution.owned_child_requires_supervisor")
                );
            }
            assert_eq!(child.effects.load(Ordering::SeqCst), 0);
            assert!(Arc::ptr_eq(
                &bus.read::<crate::nested_custody::OwnedResources<DistinctChildResources>>()
                    .unwrap()
                    .0,
                &parent
            ));
        }
    }
    #[tokio::test]
    async fn weak_parent_client_reuses_completed_slots_and_blocks_effects_after_parent_close() {
        let root = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let mut bus = Bus::new();
        bus.insert(crate::nested_custody::RequiredOwnedCustody);
        bus.insert(root.client());
        bus.insert(crate::nested_custody::OwnedResources(resources.clone()));
        let mut parent = crate::execution_observation::InvocationOwner::begin(&mut bus);
        let graph = Axon::<u32, u32, String, Resources>::start("weak-client-child").then(Effect);
        for input in [211, 212] {
            assert!(matches!(
                graph
                    .execute_owned_child(input, resources.clone(), parent.bus())
                    .await,
                crate::ExecutionReport::Unpersisted {
                    result: Outcome::Next(_),
                    ..
                }
            ));
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        drop(root);
        assert!(
            matches!(graph.execute_owned_child(213, resources.clone(), parent.bus()).await,
            crate::ExecutionReport::Unpersisted { result: Outcome::Emit(signal, _), .. }
                if signal == "execution.parent_custody_released")
        );
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        let report = parent.finish(());
        assert!(report.cleanup().is_none());
    }

    #[derive(Clone)]
    struct FailingEffect;
    #[async_trait::async_trait]
    impl Transition<u32, u32> for FailingEffect {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, _: u32, resources: &Resources, _: &mut Bus) -> Outcome<u32, String> {
            resources.effects.fetch_add(1, Ordering::SeqCst);
            Outcome::Fault("original direct fault".into())
        }
    }
    #[tokio::test]
    async fn final_owner_drop_preserves_unknown_journal_loss_and_original_observation() {
        let admission = StreamRecoveryAdmission::new(1);
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let weak = Arc::downgrade(&resources);
        let owner = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("unknown custody loss"),
            OwnedStreamContext::try_new(resources, Bus::new()).unwrap(),
            &admission,
            None,
        )
        .unwrap();
        let journal = crate::SagaRollbackJournal::new("private unknown journal");
        journal.poison_retention_for_test();
        owner.inner.context.lock().await.1.insert(journal.clone());
        let observer = owner.observe();
        let original = observer.borrow().clone();
        drop(owner);
        assert!(weak.upgrade().is_none());
        assert_eq!(admission.in_use(), Ok(0));
        let lost = observer.borrow().clone();
        assert_eq!(lost.custody, RecoveryCustody::ReleasedWithUnknownState);
        assert_eq!(lost.phase, original.phase);
        assert_eq!(lost.disposition, original.disposition);
        assert_eq!(lost.cleanup_complete, original.cleanup_complete);
        assert_eq!(lost.cleanup_timed_out, original.cleanup_timed_out);
        assert_eq!(
            journal.try_retained_work_pending(256),
            Err(crate::saga_report::JournalRetentionError::Unavailable)
        );
    }
    #[tokio::test]
    async fn live_owner_probe_times_out_restores_guard_and_retains_exact_owner() {
        let admission = StreamRecoveryAdmission::new(1);
        let mut bus = Bus::new();
        bus.insert(337u32);
        bus.set_access_policy(
            "live probe guard",
            Some(ranvier_core::BusAccessPolicy::allow_only(vec![
                ranvier_core::BusTypeRef::of::<String>(),
            ])),
        );
        let owner = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("live-owner-probe"),
            OwnedStreamContext::try_new(
                Arc::new(Resources {
                    effects: AtomicUsize::new(0),
                }),
                bus,
            )
            .unwrap(),
            &admission,
            None,
        )
        .unwrap();
        let probe = owner.recovery_probe();
        let locked = owner.inner.context.lock().await;
        assert_eq!(
            probe(tokio::time::Instant::now()).await,
            Err(crate::streaming_report_handle::StreamRecoveryProbeError::TimedOut)
        );
        drop(locked);
        let state = probe(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!state.saga_stack_nonempty);
        assert!(!state.journal_present);
        assert!(!state.recovery_pending);
        let mut context = owner.inner.context.lock().await;
        assert!(context.1.get::<u32>().is_err());
        context.1.clear_access_policy();
        assert_eq!(*context.1.read::<u32>().unwrap(), 337);
        drop(context);
        drop(owner);
        assert_eq!(admission.in_use(), Ok(1));
        drop(probe);
        assert_eq!(admission.in_use(), Ok(0));
    }

    #[test]
    fn finite_recovery_admission_precedes_effects_and_last_owner_releases_slot() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("finite-owner")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let admission = StreamRecoveryAdmission::new(1);
        let owner = OwnedStreamRecovery::try_new(
            graph.clone(),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &admission,
            None,
        )
        .unwrap();
        let rejected = match OwnedStreamRecovery::try_new(
            graph.clone(),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &admission,
            None,
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("capacity must reject before execution"),
        };
        assert_eq!(rejected.reason, RecoveryAdmissionFailure::Capacity);
        let (_, context) = rejected.into_parts();
        assert!(Arc::ptr_eq(&resources, &context.into_parts().0));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        let clone = owner.clone();
        drop(owner);
        assert_eq!(admission.in_use(), Ok(1));
        drop(clone);
        assert_eq!(admission.in_use(), Ok(0));
        let owner = OwnedStreamRecovery::try_new(
            graph.clone(),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &admission,
            None,
        )
        .unwrap();
        admission.close().unwrap();
        drop(owner);
        let rejected = match OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &admission,
            None,
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("closed admission must reject"),
        };
        assert_eq!(rejected.reason, RecoveryAdmissionFailure::Closed);
        assert_eq!(admission.in_use(), Ok(0));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn recovery_capacity_releases_after_actual_resource_drop() {
        struct Probe {
            admission: StreamRecoveryAdmission,
            observed: Arc<AtomicUsize>,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                self.observed
                    .store(self.admission.in_use().unwrap(), Ordering::SeqCst);
            }
        }
        let admission = StreamRecoveryAdmission::new(1);
        let observed = Arc::new(AtomicUsize::new(usize::MAX));
        let resources = Arc::new(Probe {
            admission: admission.clone(),
            observed: observed.clone(),
        });
        let graph = Axon::<u32, u32, String, Probe>::start("resource-release-order");
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources, Bus::new()).unwrap(),
            &admission,
            None,
        )
        .unwrap();
        drop(owner);
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        assert_eq!(admission.in_use(), Ok(0));
    }
    struct Resources {
        effects: AtomicUsize,
    }
    #[derive(Clone)]
    struct Effect;
    #[async_trait::async_trait]
    impl Transition<u32, u32> for Effect {
        type Error = String;
        type Resources = Resources;
        async fn run(
            &self,
            input: u32,
            resources: &Resources,
            _: &mut Bus,
        ) -> Outcome<u32, String> {
            resources.effects.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(input)
        }
    }
    #[derive(Clone)]
    struct Undo;
    #[async_trait::async_trait]
    impl Transition<u32, ()> for Undo {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, _: u32, resources: &Resources, bus: &mut Bus) -> Outcome<(), String> {
            assert!(!bus.cancellation_token().unwrap().is_cancelled());
            resources.effects.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(())
        }
    }
    #[test]
    fn recovery_registry_isolated_from_external_graph_mutation() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("registry-freeze")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let external = graph.saga_compensation_registry.clone();
        let node = external
            .read()
            .unwrap()
            .handlers
            .keys()
            .next()
            .unwrap()
            .clone();
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        external.write().unwrap().handlers.clear();
        assert!(
            owner
                .inner
                .graph
                .saga_compensation_registry
                .read()
                .unwrap()
                .get(&node)
                .is_some(),
            "external mutation removed recovery handler"
        );
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn poisoned_registry_rejects_before_effects_and_returns_original_authority() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("private-graph")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let original = graph.saga_compensation_registry.clone();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = original.write().unwrap();
            panic!("fixture registry poison");
        }));
        assert!(poisoned.is_err());
        let mut bus = Bus::new();
        bus.insert(String::from("private-request"));
        let admission = StreamRecoveryAdmission::new(1);
        let rejected = match OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), bus).unwrap(),
            &admission,
            None,
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("poisoned authority must not enter"),
        };
        assert_eq!(admission.in_use(), Ok(0));
        assert!(!format!("{rejected:?}").contains("private"));
        let (returned_graph, context) = rejected.into_parts();
        assert!(Arc::ptr_eq(
            &original,
            &returned_graph.saga_compensation_registry
        ));
        assert!(returned_graph.saga_compensation_registry.is_poisoned());
        let (returned_resources, bus) = context.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned_resources));
        assert_eq!(bus.read::<String>().unwrap(), "private-request");
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
    }
}
