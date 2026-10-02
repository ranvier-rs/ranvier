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

/// Local consumption observation, separate from stored prefix completion.
pub(crate) struct StreamDriveResult<T> {
    pub(crate) value: T,
    pub(crate) terminal: crate::StreamConsumptionTerminal,
}
/// Fixed late failure classification; panic payload is never returned.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamDriverError {
    Panicked,
    Cancelled(ranvier_core::CancellationContext),
}
pub(crate) struct StreamReadyResult<T> {
    pub(crate) consumption: Result<StreamDriveResult<T>, StreamDriverError>,
    pub(crate) cleanup: Option<crate::SagaRollbackReport>,
}

/// Bounded delivery: reserve before polling source, keep controls live while full,
/// and release source before returning to shielded cleanup. Completion is returned
/// directly to supervisor rather than sent behind queued items.
pub(crate) async fn produce_bounded<Item: Send + 'static>(
    source: RawStream<Item>,
    config: ranvier_core::streaming::StreamTimeoutConfig,
    token: CancellationToken,
    sender: tokio::sync::mpsc::Sender<Item>,
) -> StreamDriveResult<()> {
    use futures_util::{FutureExt, StreamExt};
    let mut observed = crate::ObservedStream::new(source, config, Some(token));
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
    StreamDriveResult {
        value: (),
        terminal,
    }
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

#[derive(Debug, Clone)]
pub(crate) struct RecoveryAbandonment {
    pub(crate) owner_id: uuid::Uuid,
    pub(crate) operator_id: uuid::Uuid,
    pub(crate) decision_id: uuid::Uuid,
    pub(crate) prior: SupervisorObservation,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RecoveryDispositionFailure {
    Missing,
    NotExpired,
    InUse,
    RecordsFull,
    InvalidIdentity,
    DuplicateDecision,
    Poisoned,
}
struct StoredRecovery<In, Out, E, Res> {
    owner: OwnedStreamRecovery<In, Out, E, Res>,
    expires: std::time::Instant,
}
/// External owner retention. Count is bounded by shared admission; payload/lifetime
/// budgets and authorized operator disposition must be added before production use.
pub(crate) struct StreamRecoveryStore<In, Out, E, Res> {
    admission: StreamRecoveryAdmission,
    retention: std::time::Duration,
    record_capacity: usize,
    records: std::sync::Mutex<Vec<RecoveryAbandonment>>,
    owners:
        std::sync::Mutex<std::collections::HashMap<uuid::Uuid, StoredRecovery<In, Out, E, Res>>>,
}
impl<In, Out, E, Res> StreamRecoveryStore<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: Send + Sync + 'static,
{
    pub(crate) fn new(capacity: usize, retention: std::time::Duration) -> Self {
        Self {
            admission: StreamRecoveryAdmission::new(capacity),
            retention,
            record_capacity: capacity,
            records: std::sync::Mutex::new(Vec::new()),
            owners: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
    /// Store owner before returning any execution handle; no effects under this lock.
    pub(crate) fn admit(
        &self,
        graph: Axon<In, Out, E, Res>,
        context: OwnedStreamContext<Res>,
    ) -> Result<
        (uuid::Uuid, OwnedStreamRecovery<In, Out, E, Res>),
        StreamRecoveryAdmissionRejected<In, Out, E, Res>,
    > {
        let mut owners = match self.owners.lock() {
            Ok(owners) => owners,
            Err(_) => {
                return Err(StreamRecoveryAdmissionRejected {
                    graph,
                    context,
                    reason: RecoveryAdmissionFailure::Poisoned,
                });
            }
        };
        let records_full = match self.records.lock() {
            Ok(records) => records.len() >= self.record_capacity,
            Err(_) => {
                return Err(StreamRecoveryAdmissionRejected {
                    graph,
                    context,
                    reason: RecoveryAdmissionFailure::Poisoned,
                });
            }
        };
        if records_full {
            return Err(StreamRecoveryAdmissionRejected {
                graph,
                context,
                reason: RecoveryAdmissionFailure::DispositionCapacity,
            });
        }
        let Some(expires) = std::time::Instant::now().checked_add(self.retention) else {
            return Err(StreamRecoveryAdmissionRejected {
                graph,
                context,
                reason: RecoveryAdmissionFailure::InvalidRetention,
            });
        };
        let owner = OwnedStreamRecovery::try_new(graph, context, &self.admission, Some(expires))?;
        let id = loop {
            let id = uuid::Uuid::new_v4();
            if let std::collections::hash_map::Entry::Vacant(entry) = owners.entry(id) {
                entry.insert(StoredRecovery {
                    owner: owner.clone(),
                    expires,
                });
                break id;
            }
            // Never replace another request's recovery custody on key collision.
        };
        Ok((id, owner))
    }
    /// Close atomically with respect to owner insertion; existing owners remain held.
    pub(crate) fn close(&self) -> Result<(), RecoveryAdmissionFailure> {
        let _owners = self
            .owners
            .lock()
            .map_err(|_| RecoveryAdmissionFailure::Poisoned)?;
        self.admission.close()
    }
    /// Explicit abandonment after expiry. Record loss before releasing sensitive
    /// custody; never classify abandonment as cleanup success or authorize retry.
    pub(crate) fn abandon_expired(
        &self,
        id: uuid::Uuid,
        operator_id: uuid::Uuid,
        decision_id: uuid::Uuid,
    ) -> Result<(), RecoveryDispositionFailure> {
        if operator_id.is_nil() || decision_id.is_nil() {
            return Err(RecoveryDispositionFailure::InvalidIdentity);
        }
        let mut owners = self
            .owners
            .lock()
            .map_err(|_| RecoveryDispositionFailure::Poisoned)?;
        let entry = owners.get(&id).ok_or(RecoveryDispositionFailure::Missing)?;
        if std::time::Instant::now() < entry.expires {
            return Err(RecoveryDispositionFailure::NotExpired);
        }
        if Arc::strong_count(&entry.owner.inner) != 1 {
            return Err(RecoveryDispositionFailure::InUse);
        }
        let mut records = self
            .records
            .lock()
            .map_err(|_| RecoveryDispositionFailure::Poisoned)?;
        if records
            .iter()
            .any(|record| record.decision_id == decision_id)
        {
            return Err(RecoveryDispositionFailure::DuplicateDecision);
        }
        if records.len() >= self.record_capacity {
            return Err(RecoveryDispositionFailure::RecordsFull);
        }
        records.push(RecoveryAbandonment {
            owner_id: id,
            operator_id,
            decision_id,
            prior: entry.owner.inner.observation.borrow().clone(),
        });
        let removed = owners.remove(&id);
        drop(records);
        drop(owners);
        drop(removed); // No user resource destructor under either store lock.
        Ok(())
    }
    /// Caller must retain reviewed external disposition evidence before acknowledging.
    /// Pair binding prevents a stale/wrong-owner acknowledgment from deleting a record.
    pub(crate) fn acknowledge_abandonment(
        &self,
        owner_id: uuid::Uuid,
        decision_id: uuid::Uuid,
    ) -> Result<bool, RecoveryDispositionFailure> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| RecoveryDispositionFailure::Poisoned)?;
        if let Some(index) = records
            .iter()
            .position(|record| record.owner_id == owner_id && record.decision_id == decision_id)
        {
            records.remove(index);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub(crate) fn abandonment_records(
        &self,
    ) -> Result<Vec<RecoveryAbandonment>, RecoveryDispositionFailure> {
        Ok(self
            .records
            .lock()
            .map_err(|_| RecoveryDispositionFailure::Poisoned)?
            .clone())
    }
    /// Explicit ownership transfer only. Removing a map entry does not release its
    /// permit or authorize replay: the returned owner remains responsible for custody.
    pub(crate) fn take_for_reconciliation(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<OwnedStreamRecovery<In, Out, E, Res>>, RecoveryAdmissionFailure> {
        Ok(self
            .owners
            .lock()
            .map_err(|_| RecoveryAdmissionFailure::Poisoned)?
            .remove(&id)
            .map(|entry| entry.owner))
    }
}

#[derive(Clone)]
pub(crate) struct ParentStreamCustody(crate::nested_custody::WeakCustody, Option<CancellationToken>);

/// A single invocation's exact graph, typed resources and request Bus. An external
/// clone must outlive the supervisor, including forced task abort. No replay API.
pub(crate) struct OwnedStreamRecovery<In, Out, E, Res> {
    inner: Arc<RecoveryInner<In, Out, E, Res>>,
}
struct RecoveryInner<In, Out, E, Res> {
    graph: Axon<In, Out, E, Res>,
    context: Mutex<(Arc<Res>, Bus)>,
    started: AtomicBool,
    expires: Option<std::time::Instant>,
    observation: tokio::sync::watch::Sender<SupervisorObservation>,
    // Last field: release reservation after graph/context fields are dropped.
    _permit: RecoveryPermit,
}
impl<In, Out, E, Res> Drop for RecoveryInner<In, Out, E, Res> {
    fn drop(&mut self) {
        // The last Arc is gone: there can be no live task/context-lock borrower.
        let (_, bus) = self.context.get_mut();
        let access = bus.take_access_scope();
        let pending = bus
            .read::<ranvier_core::saga::SagaStack>()
            .is_some_and(|stack| !stack.is_empty())
            || bus
                .read::<crate::SagaRollbackJournal>()
                .is_some_and(crate::SagaRollbackJournal::has_unresolved_tasks);
        bus.restore_access_scope(access);
        self.observation.send_modify(|state| {
            state.custody = if pending {
                RecoveryCustody::ReleasedWithPendingTasks
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
impl<In, Out, E, Res> OwnedStreamRecovery<In, Out, E, Res>
where
    In: Send + Sync + Serialize + DeserializeOwned + 'static,
    Out: Send + Sync + Serialize + DeserializeOwned + 'static,
    E: Send + Sync + Serialize + DeserializeOwned + std::fmt::Debug + 'static,
    Res: Send + Sync + 'static,
{
    pub(crate) fn try_new(
        mut graph: Axon<In, Out, E, Res>,
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
    /// Keeps InvocationOwner live until the Ready driver returns or is dropped.
    /// The internal driver must derive its terminal from actual source/control observations.
    /// Non-completed consumption runs shielded local cleanup; no replay is authorized.
    /// One explicit invocation grace bounds cooperative Saga rollback including prefix cleanup.
    /// Direct hooks, non-yielding handlers and external resource custody need separate limits.
    /// Parent owns this exact complete graph/resource/Bus context before effects.
    /// The child contains only weak links back to the authoritative parent store.
    pub(crate) async fn admit_to_parent(
        &self, client: crate::nested_custody::WeakCustodyClient,
    ) -> Result<crate::nested_custody::WeakCustody, &'static str> {
        self.admit_to_parent_with_control(client, None).await
    }

    /// Freeze inherited control at admission. A descendant can cancel locally
    /// without cancelling its parent; execution cannot replace this authority.
    pub(crate) async fn admit_to_parent_with_control(
        &self,
        client: crate::nested_custody::WeakCustodyClient,
        parent: Option<CancellationToken>,
    ) -> Result<crate::nested_custody::WeakCustody, &'static str> {
        self.bind_parent(client, parent, None, None).await
    }

    async fn bind_parent(
        &self,
        client: crate::nested_custody::WeakCustodyClient,
        parent: Option<CancellationToken>,
        journal: Option<&crate::SagaRollbackJournal>,
        task: Option<crate::nested_custody::Capability>,
    ) -> Result<crate::nested_custody::WeakCustody, &'static str> {
        if self.inner.started.load(Ordering::Acquire) {
            return Err("execution.nested_stream_already_started");
        }
        let mut context = self.inner.context.try_lock()
            .map_err(|_| "execution.nested_stream_context_busy")?;
        if self.inner.started.load(Ordering::Acquire) {
            return Err("execution.nested_stream_already_started");
        }
        let bus = &mut context.1;
        let access = bus.take_access_scope();
        let result = (|| {
            if bus.has::<ParentStreamCustody>()
                || bus.has::<crate::nested_custody::WeakCustodyClient>()
                || bus.has::<crate::nested_custody::CustodyStore>()
            {
                return Err("execution.nested_stream_already_bound");
            }
            let admission = match journal {
                Some(journal) => client.reserve_stream(self.inner.clone(), journal, task, self.observe())?,
                None => client.reserve(self.inner.clone())?,
            };
            bus.insert(client);
            bus.insert(ParentStreamCustody(admission.clone(), parent.map(|token| token.child_token())));
            Ok(admission)
        })();
        bus.restore_access_scope(access);
        result
    }

    /// Parent report retains observation, while the root store retains the actual
    /// owner. Watch receivers contain no typed resources or raw recovery inputs.
    pub(crate) async fn admit_to_parent_journal(
        &self,
        client: crate::nested_custody::WeakCustodyClient,
        journal: &crate::SagaRollbackJournal,
    ) -> Result<crate::nested_custody::WeakCustody, &'static str> {
        self.admit_to_parent_journal_with_control(client, journal, None).await
    }

    pub(crate) async fn admit_to_parent_journal_with_control(
        &self,
        client: crate::nested_custody::WeakCustodyClient,
        journal: &crate::SagaRollbackJournal,
        parent: Option<CancellationToken>,
    ) -> Result<crate::nested_custody::WeakCustody, &'static str> {
        self.bind_parent(client, parent, Some(journal), None).await
    }

    /// Register exact child owner, indexed observation and actual task holder
    /// together before the caller grants its prepared task's start permit.
    pub(crate) async fn admit_managed_to_parent<T: Send + 'static>(
        &self,
        client: crate::nested_custody::WeakCustodyClient,
        journal: &crate::SagaRollbackJournal,
        parent: CancellationToken,
        task: Arc<Mutex<crate::managed_stream_task::ManagedStreamTask<T>>>,
    ) -> Result<crate::nested_custody::WeakCustody, &'static str> {
        self.bind_parent(client, Some(parent), Some(journal), Some(task)).await
    }

    pub(crate) async fn initialize_and_drive<Item, SErr, S, T, F, Fut>(
        &self,
        input: In,
        streaming: &S,
        token: Option<CancellationToken>,
        cleanup_grace: std::time::Duration,
        drive: F,
    ) -> Result<
        StreamInitializationReport<Out, StreamReadyResult<T>, E>,
        StreamInvocationAlreadyStarted,
    >
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>,
        F: FnOnce(RawStream<Item>) -> Fut,
        Fut: Future<Output = StreamDriveResult<T>>,
    {
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
        let parent_client = owner.bus().read::<crate::nested_custody::WeakCustodyClient>().cloned();
        let inherited = parent_admission.as_ref().and_then(|admission| admission.1.clone());
        let validation = if inherited.is_some() && token.is_some() {
            Err("execution.nested_stream_control_override")
        } else { match (parent_admission, parent_client) {
            (Some(admission), Some(client)) => client.validate_member(&admission.0, &(self.inner.clone() as Arc<dyn std::any::Any + Send + Sync>)),
            (None, None) => Ok(()),
            _ => Err("execution.nested_stream_custody_mismatch"),
        } };
        if let Err(signal) = validation {
            let prefix = owner.finish(crate::ExecutionTerminal::Outcome(ranvier_core::Outcome::emit(signal, None)));
            observation.returned(SupervisorDisposition::PrefixRejected, None);
            return Ok(StreamInitializationReport::Rejected { prefix });
        }
        let parent = inherited.or(token).unwrap_or_default();
        let token = match self.inner.expires {
            Some(expires) => parent.child_with_deadline(
                expires.saturating_duration_since(std::time::Instant::now())),
            None => parent,
        };
        owner.bus().insert(crate::nested_custody::RequiredOwnedCustody);
        if !owner.bus().has::<crate::nested_custody::WeakCustodyClient>() {
            owner.bus().insert(crate::nested_custody::CustodyStore::new(128));
        }
        owner.bus().insert(crate::nested_custody::OwnedResources(resources.clone()));
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
                Some(token.clone()),
                Some(cleanup_grace),
            )
            .await;
        let mapped = match report {
            StreamInitializationReport::Ready { prefix, value } => {
                observation.phase(SupervisorPhase::Consuming);
                let saga_enabled = owner
                    .bus()
                    .read::<ranvier_core::saga::SagaPolicy>()
                    .is_some_and(|policy| *policy == ranvier_core::saga::SagaPolicy::Enabled);
                use futures_util::FutureExt as _;
                // User factory invocation is inside the boundary, as well as polling.
                let consumption = std::panic::AssertUnwindSafe(async {
                    tokio::select! {
                        biased;
                        context = token.cancelled() => Err(StreamDriverError::Cancelled(context)),
                        observed = async { drive(value).await } => Ok(observed),
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
                StreamInitializationReport::Ready {
                    prefix,
                    value: StreamReadyResult {
                        consumption,
                        cleanup,
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
mod tests {
    use super::*;
    use ranvier_core::{
        Outcome, Transition,
        saga::{SagaPolicy, SagaStack},
    };
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;
    #[tokio::test]
    async fn full_queue_observes_cancel_disconnect_and_deadline_without_extra_source_poll() {
        use std::{
            cell::Cell,
            task::{Context, Poll},
        };
        struct Source {
            polls: Arc<AtomicUsize>,
            drops: Arc<AtomicUsize>,
            entered: Arc<Notify>,
        }
        impl futures_core::Stream for Source {
            type Item = Cell<u32>;
            fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                self.polls.fetch_add(1, Ordering::SeqCst);
                self.entered.notify_one();
                Poll::Ready(Some(Cell::new(7)))
            }
        }
        impl Drop for Source {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        for mode in 0..3 {
            let polls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(Notify::new());
            let source = Source {
                polls: polls.clone(),
                drops: drops.clone(),
                entered: entered.clone(),
            };
            let token = CancellationToken::new();
            let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
            let task = tokio::spawn(produce_bounded(
                Box::pin(source),
                ranvier_core::streaming::StreamTimeoutConfig {
                    init: None,
                    idle: None,
                    total: (mode == 2).then_some(std::time::Duration::from_millis(200)),
                },
                token.clone(),
                sender,
            ));
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            if mode == 0 {
                token.cancel(ranvier_core::CancellationReason::Explicit);
            }
            if mode == 1 {
                receiver.close();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(polls.load(Ordering::SeqCst), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            match mode {
                0 => assert!(
                    matches!(result.terminal, crate::StreamConsumptionTerminal::Cancelled(context) if context.reason == ranvier_core::CancellationReason::Explicit)
                ),
                1 => assert!(
                    matches!(result.terminal, crate::StreamConsumptionTerminal::Cancelled(context) if context.reason == ranvier_core::CancellationReason::ClientDisconnected)
                ),
                _ => assert_eq!(
                    result.terminal,
                    crate::StreamConsumptionTerminal::TimedOut(crate::StreamTimeoutKind::Total)
                ),
            }
        }
    }
    #[tokio::test]
    async fn cancellation_during_ready_source_poll_prevents_item_publication() {
        struct Source(CancellationToken);
        impl futures_core::Stream for Source {
            type Item = u32;
            fn poll_next(
                self: Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<u32>> {
                self.0.cancel(ranvier_core::CancellationReason::Explicit);
                std::task::Poll::Ready(Some(7))
            }
        }
        let token = CancellationToken::new();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let result = produce_bounded(
            Box::pin(Source(token.clone())),
            ranvier_core::streaming::StreamTimeoutConfig {
                init: None,
                idle: None,
                total: None,
            },
            token,
            sender,
        )
        .await;
        assert!(matches!(
            result.terminal,
            crate::StreamConsumptionTerminal::Cancelled(_)
        ));
        assert!(receiver.recv().await.is_none());
    }
    struct QueueInit(Arc<Notify>);
    #[async_trait::async_trait]
    impl StreamingTransition<u32> for QueueInit {
        type Item = u32;
        type Error = String;
        type Resources = Resources;
        async fn run_stream(
            &self,
            _: u32,
            _: &Resources,
            _: &mut Bus,
        ) -> Result<RawStream<u32>, String> {
            use futures_util::StreamExt;
            let entered = self.0.clone();
            Ok(Box::pin(
                futures_util::stream::once(async move {
                    entered.notify_one();
                    7
                })
                .chain(futures_util::stream::pending()),
            ))
        }
    }
    #[tokio::test]
    async fn full_queue_consumer_close_runs_actual_shielded_saga_cleanup() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("queue-close-cleanup")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let entered = Arc::new(Notify::new());
        let task_entered = entered.clone();
        let token = CancellationToken::new();
        let driver_token = token.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let mut observer = owner.observe();
        let task_owner = owner.clone();
        let task = tokio::spawn(async move {
            task_owner
                .initialize_and_drive(
                    43,
                    &QueueInit(task_entered),
                    Some(token),
                    std::time::Duration::from_secs(5),
                    move |source| {
                        produce_bounded(
                            source,
                            ranvier_core::streaming::StreamTimeoutConfig {
                                init: None,
                                idle: None,
                                total: None,
                            },
                            driver_token,
                            sender,
                        )
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        receiver.close();
        let report = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        let Ok(StreamInitializationReport::Ready { value, .. }) = report else {
            panic!("expected initialized consumption");
        };
        let consumption = value.consumption.unwrap();
        assert!(
            matches!(consumption.terminal, crate::StreamConsumptionTerminal::Cancelled(context) if context.reason == ranvier_core::CancellationReason::ClientDisconnected)
        );
        assert!(value.cleanup.unwrap().is_complete());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        assert!(
            owner
                .inner
                .context
                .lock()
                .await
                .1
                .read::<SagaStack>()
                .unwrap()
                .is_empty()
        );
        // Completion is observable without draining the already queued item.
        assert_eq!(receiver.len(), 1);
        assert_eq!(
            observer.borrow_and_update().phase,
            SupervisorPhase::Returned
        );
        assert_eq!(observer.borrow().cleanup_complete, Some(true));
        drop(owner);
        assert_eq!(
            observer.borrow().custody,
            RecoveryCustody::ReleasedWithoutRetainedTasks
        );
        assert_eq!(observer.borrow().cleanup_complete, Some(true));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Returned);
        assert!(matches!(
            &observer.borrow().disposition,
            Some(SupervisorDisposition::Consumption(
                crate::StreamConsumptionTerminal::Cancelled(_)
            ))
        ));
    }
    struct PanicInit(usize);
    struct PanicSource(usize);
    impl futures_core::Stream for PanicSource {
        type Item = u32;
        fn poll_next(
            self: Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<u32>> {
            if self.0 == 1 || self.0 == 4 {
                panic!("private-source-poll-payload");
            }
            std::task::Poll::Ready(None)
        }
    }
    impl Drop for PanicSource {
        fn drop(&mut self) {
            if self.0 == 2 {
                panic!("private-source-drop-payload");
            }
        }
    }
    #[async_trait::async_trait]
    impl StreamingTransition<u32> for PanicInit {
        type Item = u32;
        type Error = String;
        type Resources = Resources;
        async fn run_stream(
            &self,
            _: u32,
            _: &Resources,
            _: &mut Bus,
        ) -> Result<RawStream<u32>, String> {
            Ok(Box::pin(PanicSource(self.0)))
        }
    }
    #[tokio::test]
    async fn late_factory_poll_and_drop_panics_return_fixed_failure_and_actual_cleanup() {
        for mode in 0..5 {
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let prefix = Axon::<u32, u32, String, Resources>::start("late-panic")
                .with_saga_policy(SagaPolicy::Enabled);
            let graph = if mode == 4 {
                prefix.then_compensated(Effect, FaultUndo)
            } else {
                prefix.then_compensated(Effect, Undo)
            };
            let owner = OwnedStreamRecovery::try_new(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1),
                None,
            )
            .unwrap();
            let (sender, _receiver) = tokio::sync::mpsc::channel(1);
            let report = owner
                .initialize_and_drive(
                    47,
                    &PanicInit(mode),
                    None,
                    std::time::Duration::from_secs(5),
                    move |source| {
                        if mode == 0 {
                            panic!("private-driver-factory-payload");
                        }
                        async move {
                            if mode == 3 {
                                panic!("private-driver-poll-payload");
                            }
                            produce_bounded(
                                source,
                                ranvier_core::streaming::StreamTimeoutConfig {
                                    init: None,
                                    idle: None,
                                    total: None,
                                },
                                CancellationToken::new(),
                                sender,
                            )
                            .await
                        }
                    },
                )
                .await;
            let Ok(StreamInitializationReport::Ready { prefix, value }) = report else {
                panic!("expected late failure after initialization");
            };
            assert!(matches!(prefix, crate::ExecutionReport::Unpersisted { .. }));
            assert!(matches!(
                value.consumption,
                Err(StreamDriverError::Panicked)
            ));
            assert_eq!(format!("{:?}", StreamDriverError::Panicked), "Panicked");
            let cleanup = value.cleanup.unwrap();
            assert_eq!(cleanup.is_complete(), mode != 4);
            assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
            let context = owner.inner.context.lock().await;
            assert!(context.1.read::<SagaStack>().unwrap().is_empty());
            let retained = context
                .1
                .read::<crate::SagaRollbackJournal>()
                .unwrap()
                .unresolved_tasks();
            assert_eq!(retained.len(), usize::from(mode == 4));
            if mode == 4 {
                assert_eq!(retained[0].input_snapshot, b"47");
                assert_eq!(retained[0].node_id, cleanup.steps[0].node_id);
            }
        }
    }
    #[derive(Clone)]
    struct AdvancingEffect;
    #[async_trait::async_trait]
    impl Transition<u32, u32> for AdvancingEffect {
        type Error = String;
        type Resources = Resources;
        async fn run(
            &self,
            input: u32,
            resources: &Resources,
            _: &mut Bus,
        ) -> Outcome<u32, String> {
            resources.effects.fetch_add(1, Ordering::SeqCst);
            Outcome::Next(input + 1)
        }
    }
    #[derive(Clone)]
    struct PendingUndo(
        Arc<Notify>,
        Arc<std::sync::Mutex<Option<crate::SagaRollbackJournal>>>,
    );
    #[async_trait::async_trait]
    impl Transition<u32, ()> for PendingUndo {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, _: u32, _: &Resources, bus: &mut Bus) -> Outcome<(), String> {
            assert!(!bus.cancellation_token().unwrap().is_cancelled());
            *self.1.lock().unwrap() = bus.read::<crate::SagaRollbackJournal>().cloned();
            self.0.notify_one();
            bus.set_access_policy(
                "pending-undo",
                Some(ranvier_core::bus::BusAccessPolicy::allow_only(Vec::new())),
            );
            std::future::pending().await
        }
    }
    struct InterruptedExternal {
        marker_phase: bool,
        entered: Arc<Notify>,
        calls: AtomicUsize,
        applied: AtomicBool,
    }
    #[async_trait::async_trait]
    impl crate::persistence::CompensationHook for InterruptedExternal {
        async fn compensate(
            &self,
            _: crate::persistence::CompensationContext,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.marker_phase {
                return Ok(());
            }
            self.entered.notify_one();
            std::future::pending().await
        }
    }
    #[async_trait::async_trait]
    impl crate::persistence::CompensationIdempotencyStore for InterruptedExternal {
        async fn was_compensated(&self, _: &str) -> anyhow::Result<bool> {
            Ok(self.applied.load(Ordering::SeqCst))
        }
        async fn mark_compensated(&self, _: &str) -> anyhow::Result<()> {
            self.applied.store(true, Ordering::SeqCst);
            self.entered.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn actual_external_hook_and_applied_marker_abort_preserve_store_custody() {
        use crate::persistence::{
            CompensationHandle, CompensationIdempotencyHandle, InMemoryPersistenceStore,
            PersistenceHandle, PersistenceStore, PersistenceTraceId,
        };
        for marker_phase in [false, true] {
            let entered = Arc::new(Notify::new());
            let capability = Arc::new(InterruptedExternal {
                marker_phase,
                entered: entered.clone(),
                calls: AtomicUsize::new(0),
                applied: AtomicBool::new(false),
            });
            let hook: Arc<dyn crate::persistence::CompensationHook> = capability.clone();
            let marker: Arc<dyn crate::persistence::CompensationIdempotencyStore> =
                capability.clone();
            let persistence = Arc::new(InMemoryPersistenceStore::new());
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let mut bus = Bus::new();
            bus.insert(PersistenceHandle::from_arc(persistence.clone()));
            bus.insert(PersistenceTraceId::new("external-abort-trace"));
            bus.insert(CompensationHandle::from_arc(hook.clone()));
            bus.insert(CompensationIdempotencyHandle::from_arc(marker.clone()));
            let store = StreamRecoveryStore::new(1, std::time::Duration::from_secs(60));
            let (id, owner) = store
                .admit(
                    Axon::<u32, u32, String, Resources>::start("external-abort-circuit")
                        .then(FailingEffect),
                    OwnedStreamContext::try_new(resources.clone(), bus).unwrap(),
                )
                .unwrap();
            let observer = owner.observe();
            let task = tokio::spawn(async move {
                owner
                    .initialize_and_drive(
                        129,
                        &Init,
                        None,
                        std::time::Duration::from_secs(30),
                        |_| async {
                            panic!("failed prefix must not start driver");
                            #[allow(unreachable_code)]
                            completed()
                        },
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
            assert!(!observer.borrow().cleanup_timed_out);
            assert_eq!(observer.borrow().cleanup_complete, None);
            assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
            assert_eq!(store.admission.in_use(), Ok(1));
            let retained = store.take_for_reconciliation(id).unwrap().unwrap();
            {
                let context = retained.inner.context.lock().await;
                assert!(Arc::ptr_eq(&resources, &context.0));
                let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
                let pending = journal.unresolved_external();
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].context.trace_id, "external-abort-trace");
                assert_eq!(pending[0].context.circuit, "external-abort-circuit");
                assert!(Arc::ptr_eq(&pending[0].handle.hook(), &hook));
                assert!(pending[0].invoked.load(Ordering::SeqCst));
                assert!(pending[0].idempotency.as_ref().unwrap().shares_store_with(&marker));
                assert!(!format!("{journal:?}").contains("external-abort-circuit"));
                assert!(!journal.snapshot().finished);
                assert_eq!(
                    journal.snapshot().steps[0].outcome,
                    crate::SagaRollbackOutcome::InProgress
                );
            }
            let interrupted = observer.borrow().clone();
            assert!(matches!(
                retained
                    .initialize_and_drive(
                        130,
                        &Init,
                        None,
                        std::time::Duration::from_secs(30),
                        |_| async { completed() }
                    )
                    .await,
                Err(StreamInvocationAlreadyStarted)
            ));
            assert_eq!(*observer.borrow(), interrupted);
            assert_eq!(capability.calls.load(Ordering::SeqCst), 1);
            assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
            assert_eq!(capability.applied.load(Ordering::SeqCst), marker_phase);
            let trace = persistence
                .load("external-abort-trace")
                .await
                .unwrap()
                .unwrap();
            assert!(!trace
                .events
                .iter()
                .any(|event| event.outcome_kind == "Compensated"));
            assert_eq!(trace.completion, None);
            assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
            drop(retained);
            assert_eq!(
                observer.borrow().custody,
                RecoveryCustody::ReleasedWithPendingTasks
            );
            assert_eq!(store.admission.in_use(), Ok(0));
        }
    }
    #[derive(Clone)]
    struct NestedExternalFault {
        hook: Arc<dyn crate::persistence::CompensationHook>,
        parent: Arc<std::sync::Mutex<Option<crate::SagaRollbackJournal>>>,
        child_trace: Arc<std::sync::Mutex<Option<String>>>,
        seed_parent: bool,
        incomplete: bool,
    }
    #[async_trait::async_trait]
    impl Transition<u32, u32> for NestedExternalFault {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, input: u32, resources: &Resources, bus: &mut Bus) -> Outcome<u32, String> {
            if self.seed_parent {
                let trace = bus
                    .read::<crate::execution_observation::InvocationTrace>()
                    .unwrap()
                    .0
                    .clone();
                let parent = crate::SagaRollbackJournal::new(&trace);
                parent.push(
                    "parent-completed".into(),
                    crate::SagaRollbackOutcome::Completed,
                );
                parent.finish();
                bus.insert(parent.clone());
                *self.parent.lock().unwrap() = Some(parent);
            }
            let original = bus.remove::<crate::persistence::CompensationHandle>();
            bus.insert(crate::persistence::CompensationHandle::from_arc(
                self.hook.clone(),
            ));
            let child = Axon::<u32, u32, String, Resources>::start("private-child-external");
            let token = CancellationToken::new();
            token.cancel(ranvier_core::CancellationReason::Explicit);
            let report = child
                .execute_cancellable(input, resources, bus, token)
                .await;
            assert_eq!(!report.cleanup().unwrap().is_complete(), self.incomplete);
            *self.child_trace.lock().unwrap() = Some(report.cleanup().unwrap().trace_id.clone());
            bus.remove::<crate::persistence::CompensationHandle>();
            if let Some(original) = original {
                bus.insert(original);
            }
            Outcome::Next(input)
        }
    }
    #[tokio::test]
    async fn nested_unresolved_external_child_prevents_parent_stream_admission() {
        for seed_parent in [false, true] {
            for incomplete in [false, true] {
                let calls = Arc::new(AtomicUsize::new(0));
                let hook: Arc<dyn crate::persistence::CompensationHook> = Arc::new(ExternalHook {
                    mode: if incomplete { 4 } else { 7 },
                    calls: calls.clone(),
                });
                let parent = Arc::new(std::sync::Mutex::new(None));
                let child_trace = Arc::new(std::sync::Mutex::new(None));
                let resources = Arc::new(Resources {
                    effects: AtomicUsize::new(0),
                });
                let owner = OwnedStreamRecovery::try_new(
                    Axon::<u32, u32, String, Resources>::start("parent-external").then(
                        NestedExternalFault {
                            hook: hook.clone(),
                            parent: parent.clone(),
                            child_trace: child_trace.clone(),
                            seed_parent,
                            incomplete,
                        },
                    ),
                    OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                    &StreamRecoveryAdmission::new(1),
                    None,
                )
                .unwrap();
                let drivers = Arc::new(AtomicUsize::new(0));
                let count = drivers.clone();
                let report = owner
                    .initialize_and_drive(
                        137,
                        &Init,
                        None,
                        std::time::Duration::from_secs(5),
                        move |_| async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            completed()
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    matches!(report, StreamInitializationReport::Rejected { .. }),
                    incomplete
                );
                assert_eq!(drivers.load(Ordering::SeqCst), usize::from(!incomplete));
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                {
                    let context = owner.inner.context.lock().await;
                    assert!(Arc::ptr_eq(&context.0, &resources));
                    if incomplete {
                        let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
                        let children = journal.retained_children();
                        assert_eq!(children.len(), 1);
                        assert_eq!(
                            journal.snapshot().steps[children[0].0].node_id,
                            "nested-invocation"
                        );
                        assert_ne!(
                            children[0].1.snapshot().trace_id,
                            journal.snapshot().trace_id
                        );
                        assert_eq!(
                            children[0].1.snapshot().trace_id,
                            *child_trace.lock().unwrap().as_ref().unwrap()
                        );
                        let pending = children[0].1.unresolved_external();
                        assert_eq!(pending.len(), 1);
                        assert_eq!(pending[0].context.circuit, "private-child-external");
                        assert!(Arc::ptr_eq(&pending[0].handle.hook(), &hook));
                        assert!(journal.has_unresolved_tasks());
                        assert!(!journal.snapshot().is_complete());
                        assert!(!format!("{journal:?}").contains("private-child-external"));
                        if seed_parent {
                            let original = parent.lock().unwrap();
                            assert!(original.as_ref().unwrap().shares_report_with(journal));
                            assert_eq!(journal.snapshot().steps[0].node_id, "parent-completed");
                            assert_eq!(
                                journal.snapshot().steps[0].outcome,
                                crate::SagaRollbackOutcome::Completed
                            );
                        }
                        match &report {
                            StreamInitializationReport::Rejected { prefix } => assert!(matches!(
                                prefix,
                                crate::ExecutionReport::Unpersisted {
                                    result: crate::ExecutionTerminal::Outcome(Outcome::Next(137)),
                                    ..
                                }
                            )),
                            _ => panic!("unknown child admitted"),
                        }
                    } else if let Some(journal) = context.1.read::<crate::SagaRollbackJournal>() {
                        assert!(journal.retained_children().is_empty());
                        assert!(!journal.has_unresolved_tasks());
                    }
                }
                let observer = owner.observe();
                drop(owner);
                assert_eq!(
                    observer.borrow().custody,
                    if incomplete {
                        RecoveryCustody::ReleasedWithPendingTasks
                    } else {
                        RecoveryCustody::ReleasedWithoutRetainedTasks
                    }
                );
            }
        }
    }
    #[tokio::test]
    async fn actual_abort_during_nested_external_hook_retains_child_under_parent_store_owner() {
        let entered = Arc::new(Notify::new());
        let capability = Arc::new(InterruptedExternal {
            marker_phase: false,
            entered: entered.clone(),
            calls: AtomicUsize::new(0),
            applied: AtomicBool::new(false),
        });
        let hook: Arc<dyn crate::persistence::CompensationHook> = capability.clone();
        let parent = Arc::new(std::sync::Mutex::new(None));
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let store = StreamRecoveryStore::new(1, std::time::Duration::from_secs(60));
        let (id, owner) = store
            .admit(
                Axon::<u32, u32, String, Resources>::start("parent-abort").then(NestedExternalFault {
                    hook: hook.clone(),
                    parent: parent.clone(),
                    child_trace: Arc::new(std::sync::Mutex::new(None)),
                    seed_parent: true,
                    incomplete: true,
                }),
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            )
            .unwrap();
        let observer = owner.observe();
        let task = tokio::spawn(async move {
            owner
                .initialize_and_drive(
                    139,
                    &Init,
                    None,
                    std::time::Duration::from_secs(30),
                    |_| async {
                        panic!("pending child must not start driver");
                        #[allow(unreachable_code)]
                        completed()
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        assert!(!observer.borrow().cleanup_timed_out);
        let retained = store.take_for_reconciliation(id).unwrap().unwrap();
        {
            let context = retained.inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
            assert!(parent
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .shares_report_with(journal));
            let children = journal.retained_children();
            assert_eq!(children.len(), 1);
            assert_eq!(
                journal.snapshot().steps[children[0].0].node_id,
                "nested-invocation"
            );
            let pending = children[0].1.unresolved_external();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].context.circuit, "private-child-external");
            assert_ne!(pending[0].context.trace_id, journal.snapshot().trace_id);
            assert!(Arc::ptr_eq(&pending[0].handle.hook(), &hook));
            assert_eq!(
                children[0].1.snapshot().steps[0].outcome,
                crate::SagaRollbackOutcome::InProgress
            );
            assert!(journal.has_unresolved_tasks());
        }
        assert_eq!(capability.calls.load(Ordering::SeqCst), 1);
        drop(retained);
        assert_eq!(
            observer.borrow().custody,
            RecoveryCustody::ReleasedWithPendingTasks
        );
    }
    #[derive(Clone)]
    struct DropNestedSaga;
    #[async_trait::async_trait]
    impl Transition<u32, u32> for DropNestedSaga {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, input: u32, resources: &Resources, bus: &mut Bus) -> Outcome<u32, String> {
            let entered = Arc::new(Notify::new());
            let child = Axon::<u32, u32, String, Resources>::start("nested-saga-remaining")
                .with_saga_policy(SagaPolicy::Enabled)
                .then_compensated(AdvancingEffect, Undo)
                .then_compensated(
                    AdvancingEffect,
                    PendingUndo(entered.clone(), Arc::new(std::sync::Mutex::new(None))),
                )
                .then(FailingEffect);
            {
                let execution = child.execute(input, resources, bus);
                tokio::pin!(execution);
                tokio::select! {
                    _ = entered.notified() => { child.saga_compensation_registry.write().unwrap().handlers.clear(); },
                    _ = &mut execution => panic!("pending child undo returned"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("child undo did not start"),
                }
            }
            Outcome::Next(input)
        }
    }
    #[tokio::test]
    async fn nested_pending_undo_retains_popped_and_remaining_child_inputs() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let owner = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("parent-remaining").then(DropNestedSaga),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let report = owner
            .initialize_and_drive(
                171,
                &Init,
                None,
                std::time::Duration::from_secs(5),
                |_| async {
                    panic!("interrupted child must reject admission");
                    #[allow(unreachable_code)]
                    completed()
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            report,
            StreamInitializationReport::Rejected { .. }
        ));
        let context = owner.inner.context.lock().await;
        let children = context
            .1
            .read::<crate::SagaRollbackJournal>()
            .unwrap()
            .retained_children();
        assert_eq!(children.len(), 1);
        let child = &children[0].1;
        let custody = child.retained_custody().unwrap()
            .downcast::<crate::nested_custody::NestedSagaCustody<String, Resources>>().unwrap_or_else(|_| panic!("wrong typed custody"));
        assert!(Arc::ptr_eq(&custody.resources, &resources));
        let pending = child.unresolved_tasks();
        assert_eq!(
            pending.len(),
            2,
            "remaining child stack must not be discarded"
        );
        assert_eq!(pending[0].input_snapshot, b"172");
        assert_eq!(pending[1].input_snapshot, b"171");
        assert!(pending.iter().all(|task| custody.handlers.get(&task.node_id).is_some()));
        let report = child.snapshot();
        assert!(!report.finished);
        assert_eq!(
            report.steps[0].outcome,
            crate::SagaRollbackOutcome::InProgress
        );
        assert_eq!(
            report.steps[1].outcome,
            crate::SagaRollbackOutcome::NotInvoked
        );
        let weak = Arc::downgrade(&resources);
        drop(context);
        drop(resources);
        drop(custody);
        drop(owner);
        // A handler in this fixture captures the public journal. Its weak custody
        // edge must not keep the typed resource/store alive after root release.
        assert!(weak.upgrade().is_none());
        assert!(child.retained_custody().is_none());
    }
    #[tokio::test]
    async fn required_nested_custody_rejects_mismatch_capacity_and_busy_registry_before_effects() {
        for mode in 0..3 {
            let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
            let graph = Axon::<u32, u32, String, Resources>::start("custody-pre-effect").then(FailingEffect);
            let mut bus = Bus::new();
            bus.insert(crate::nested_custody::RequiredOwnedCustody);
            let configured = if mode == 0 { Arc::new(Resources { effects: AtomicUsize::new(0) }) } else { resources.clone() };
            bus.insert(crate::nested_custody::OwnedResources(configured));
            bus.insert(crate::nested_custody::CustodyStore::new(if mode == 1 { 0 } else { 1 }));
            bus.set_access_policy("deny-framework-fixture", Some(ranvier_core::BusAccessPolicy::allow_only(Vec::new())));
            let held = if mode == 2 { Some(graph.saga_compensation_registry.write().unwrap()) } else { None };
            let report = graph.execute(181, resources.as_ref(), &mut bus).await;
            assert!(matches!(report, crate::ExecutionReport::Unpersisted { result: Outcome::Emit(signal, _), .. }
                if signal == match mode { 0 => "execution.nested_resources_unowned", 1 => "execution.nested_custody_capacity", _ => "execution.nested_registry_unavailable" }));
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
    #[derive(Clone)]
    struct DistinctChildUndo(Arc<Notify>);
    #[async_trait::async_trait]
    impl Transition<u32, ()> for DistinctChildUndo {
        type Error = String;
        type Resources = DistinctChildResources;
        async fn run(&self, _: u32, _: &DistinctChildResources, bus: &mut Bus) -> Outcome<(), String> {
            assert!(!bus.cancellation_token().unwrap().is_cancelled());
            self.0.notify_one();
            std::future::pending().await
        }
    }
    #[derive(Clone)]
    struct DistinctChildStop(Option<CancellationToken>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for DistinctChildStop {
        type Error = String;
        type Resources = DistinctChildResources;
        async fn run(&self, _: u32, _: &DistinctChildResources, _: &mut Bus) -> Outcome<u32, String> {
            if let Some(token) = &self.0 {
                token.cancel(ranvier_core::CancellationReason::Explicit);
                std::future::pending().await
            } else {
                Outcome::Fault("child fault".into())
            }
        }
    }
    #[derive(Clone)]
    struct DistinctOwnedChild {
        cancellable: bool,
        outer_abort: bool,
        reached: Arc<Notify>,
        allocation: Arc<std::sync::Mutex<Option<std::sync::Weak<DistinctChildResources>>>>,
    }
    #[async_trait::async_trait]
    impl Transition<u32, u32> for DistinctOwnedChild {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, input: u32, resources: &Resources, bus: &mut Bus) -> Outcome<u32, String> {
            let allocation = Arc::new(DistinctChildResources {
                effects: AtomicUsize::new(0),
            });
            *self.allocation.lock().unwrap() = Some(Arc::downgrade(&allocation));
            let entered = Arc::new(Notify::new());
            let token = CancellationToken::new();
            let child = Axon::<u32, u32, String, DistinctChildResources>::start("distinct-child")
                .with_saga_policy(SagaPolicy::Enabled)
                .then_compensated(DistinctChildEffect, DistinctChildUndo(entered.clone()))
                .then(DistinctChildStop(self.cancellable.then_some(token.clone())));
            {
                let execution = async {
                    if self.cancellable {
                        let _unexpected_report = child
                            .execute_owned_child_cancellable(input, allocation.clone(), bus, token)
                            .await;
                    } else {
                        let _unexpected_report = child
                            .execute_owned_child(input, allocation.clone(), bus)
                            .await;
                    }
                    panic!("pending distinct child returned a report");
                };
                tokio::pin!(execution);
                tokio::select! {
                    _ = entered.notified() => {
                        self.reached.notify_one();
                        if self.outer_abort { std::future::pending::<()>().await; }
                    },
                    _ = &mut execution => panic!("pending distinct child returned"),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("distinct child did not enter undo"),
                }
            }
            assert!(bus
                .read::<crate::nested_custody::OwnedResources<DistinctChildResources>>()
                .is_none());
            assert!(std::ptr::eq(
                bus.read::<crate::nested_custody::OwnedResources<Resources>>()
                    .unwrap()
                    .0
                    .as_ref(),
                resources
            ));
            Outcome::Next(input)
        }
    }
    #[tokio::test]
    async fn distinct_owned_child_resource_and_handlers_survive_child_future_drop_and_owner_transfer() {
        for cancellable in [false, true] {
            let allocation = Arc::new(std::sync::Mutex::new(None));
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let store = StreamRecoveryStore::new(1, std::time::Duration::from_secs(60));
            let (id, owner) = store
                .admit(
                    Axon::<u32, u32, String, Resources>::start("distinct-parent").then(
                        DistinctOwnedChild {
                            cancellable,
                            outer_abort: false,
                            reached: Arc::new(Notify::new()),
                            allocation: allocation.clone(),
                        },
                    ),
                    OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                )
                .unwrap();
            let report = owner
                .initialize_and_drive(
                    193,
                    &Init,
                    None,
                    std::time::Duration::from_secs(5),
                    |_| async {
                        panic!("pending owned child must reject streaming admission");
                        #[allow(unreachable_code)]
                        completed()
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                report,
                StreamInitializationReport::Rejected { .. }
            ));
            drop(owner);
            let retained = store.take_for_reconciliation(id).unwrap().unwrap();
            let weak = allocation.lock().unwrap().as_ref().unwrap().clone();
            {
                let context = retained.inner.context.lock().await;
                let children = context
                    .1
                    .read::<crate::SagaRollbackJournal>()
                    .unwrap()
                    .retained_children();
                assert_eq!(children.len(), 1);
                let child = &children[0].1;
                let custody = child.retained_custody().unwrap().downcast::<crate::nested_custody::NestedSagaCustody<String, DistinctChildResources>>()
                    .unwrap_or_else(|_| panic!("wrong child resource type"));
                assert!(Arc::ptr_eq(&custody.resources, &weak.upgrade().unwrap()));
                assert_eq!(custody.resources.effects.load(Ordering::SeqCst), 1);
                let tasks = child.unresolved_tasks();
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].input_snapshot, b"193");
                assert!(custody.handlers.get(&tasks[0].node_id).is_some());
                assert_eq!(
                    child.snapshot().steps[0].outcome,
                    crate::SagaRollbackOutcome::InProgress
                );
                assert!(context
                    .1
                    .read::<crate::nested_custody::OwnedResources<DistinctChildResources>>()
                    .is_none());
            }
            assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
            let observer = retained.observe();
            drop(retained);
            assert!(weak.upgrade().is_none());
            assert_eq!(
                observer.borrow().custody,
                RecoveryCustody::ReleasedWithPendingTasks
            );
        }
    }
    #[tokio::test]
    async fn actual_root_abort_retains_distinct_owned_child_until_root_store_transfer() {
        for cancellable in [false, true] {
            let allocation = Arc::new(std::sync::Mutex::new(None));
            let reached = Arc::new(Notify::new());
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let store = StreamRecoveryStore::new(1, std::time::Duration::from_secs(60));
            let (id, owner) = store
                .admit(
                    Axon::<u32, u32, String, Resources>::start("distinct-abort-parent").then(
                        DistinctOwnedChild {
                            cancellable,
                            outer_abort: true,
                            reached: reached.clone(),
                            allocation: allocation.clone(),
                        },
                    ),
                    OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                )
                .unwrap();
            let observer = owner.observe();
            let task = tokio::spawn(async move {
                owner
                    .initialize_and_drive(
                        199,
                        &Init,
                        None,
                        std::time::Duration::from_secs(30),
                        |_| async {
                            panic!("pending child must not start driver");
                            #[allow(unreachable_code)]
                            completed()
                        },
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), reached.notified())
                .await
                .unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
            assert!(!observer.borrow().cleanup_timed_out);
            assert_eq!(observer.borrow().cleanup_complete, None);
            assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
            let retained = store.take_for_reconciliation(id).unwrap().unwrap();
            let weak = allocation.lock().unwrap().as_ref().unwrap().clone();
            {
                let context = retained.inner.context.lock().await;
                assert!(Arc::ptr_eq(&context.0, &resources));
                assert!(Arc::ptr_eq(
                    &context
                        .1
                        .read::<crate::nested_custody::OwnedResources<Resources>>()
                        .unwrap()
                        .0,
                    &resources
                ));
                assert!(context
                    .1
                    .read::<crate::nested_custody::OwnedResources<DistinctChildResources>>()
                    .is_none());
                let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
                let children = journal.retained_children();
                assert_eq!(children.len(), 1);
                let child = &children[0].1;
                assert_ne!(child.snapshot().trace_id, journal.snapshot().trace_id);
                let custody = child.retained_custody().unwrap().downcast::<crate::nested_custody::NestedSagaCustody<String, DistinctChildResources>>()
                    .unwrap_or_else(|_| panic!("wrong child resource type"));
                assert!(Arc::ptr_eq(&custody.resources, &weak.upgrade().unwrap()));
                assert_eq!(custody.resources.effects.load(Ordering::SeqCst), 1);
                let tasks = child.unresolved_tasks();
                assert_eq!(tasks[0].input_snapshot, b"199");
                assert!(custody.handlers.get(&tasks[0].node_id).is_some());
                assert_eq!(
                    child.snapshot().steps[0].outcome,
                    crate::SagaRollbackOutcome::InProgress
                );
            }
            let before = observer.borrow().clone();
            assert!(matches!(
                retained
                    .initialize_and_drive(
                        200,
                        &Init,
                        None,
                        std::time::Duration::from_secs(30),
                        |_| async { completed() }
                    )
                    .await,
                Err(StreamInvocationAlreadyStarted)
            ));
            assert_eq!(*observer.borrow(), before);
            drop(retained);
            assert!(weak.upgrade().is_none());
            assert_eq!(
                observer.borrow().custody,
                RecoveryCustody::ReleasedWithPendingTasks
            );
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
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let mut bus = Bus::new();
        bus.insert(crate::nested_custody::RequiredOwnedCustody);
        bus.insert(root.client());
        bus.insert(crate::nested_custody::OwnedResources(resources.clone()));
        let mut parent = crate::execution_observation::InvocationOwner::begin(&mut bus);
        let graph = Axon::<u32, u32, String, Resources>::start("weak-client-child").then(Effect);
        for input in [211, 212] {
            assert!(matches!(graph.execute_owned_child(input, resources.clone(), parent.bus()).await,
                crate::ExecutionReport::Unpersisted { result: Outcome::Next(_), .. }));
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        drop(root);
        assert!(matches!(graph.execute_owned_child(213, resources.clone(), parent.bus()).await,
            crate::ExecutionReport::Unpersisted { result: Outcome::Emit(signal, _), .. }
                if signal == "execution.parent_custody_released"));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        let report = parent.finish(());
        assert!(report.cleanup().is_none());
    }
    #[tokio::test]
    async fn parent_store_retains_full_stream_child_graph_resources_bus_after_actual_driver_abort() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let weak = Arc::downgrade(&resources);
        let mut bus = Bus::new();
        bus.insert(9876u64);
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("full-owned-child").with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo),
            OwnedStreamContext::try_new(resources.clone(), bus).unwrap(), &StreamRecoveryAdmission::new(1), None).unwrap();
        let parent_journal = crate::SagaRollbackJournal::new("parent-stream-report");
        parent_journal.push("prior-parent-hook".into(), crate::SagaRollbackOutcome::Completed);
        let parent_clone = parent_journal.clone();
        let admission = child.admit_to_parent_journal(parent.client(), &parent_journal).await.unwrap();
        assert!(parent_journal.has_unresolved_tasks());
        assert!(!parent_journal.snapshot().is_complete());
        assert!(matches!(parent.reserve(Arc::new(())), Err("execution.nested_custody_capacity")));
        let observer = child.observe();
        let entered = Arc::new(Notify::new());
        let signal = entered.clone();
        let task = tokio::spawn(async move {
            child.initialize_and_drive(233, &Init, None, std::time::Duration::from_secs(30), move |source| async move {
                let _source = source;
                signal.notify_one();
                std::future::pending::<StreamDriveResult<()>>().await
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        let observations = parent_journal.retained_stream_observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].0, 1);
        assert_eq!(parent_clone.retained_stream_observations()[0].0, 1);
        assert_eq!(parent_journal.snapshot().steps[0].outcome, crate::SagaRollbackOutcome::Completed);
        assert_eq!(observations[0].1.phase, SupervisorPhase::Interrupted);
        assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
        let inner = admission.upgrade().unwrap().downcast::<RecoveryInner<u32, u32, String, Resources>>()
            .unwrap_or_else(|_| panic!("wrong full child owner"));
        let retained = OwnedStreamRecovery { inner };
        {
            let context = retained.inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            assert_eq!(context.1.read::<u64>(), Some(&9876));
            assert!(context.1.has::<crate::nested_custody::WeakCustodyClient>());
            assert!(!context.1.has::<crate::nested_custody::CustodyStore>());
            let task = &context.1.read::<SagaStack>().unwrap().tasks[0];
            assert_eq!(task.input_snapshot, b"233");
            assert!(retained.inner.graph.saga_compensation_registry.read().unwrap().get(&task.node_id).is_some());
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        drop(resources);
        drop(retained);
        assert!(weak.upgrade().is_some());
        drop(parent);
        assert!(weak.upgrade().is_none());
        assert_eq!(observer.borrow().custody, RecoveryCustody::ReleasedWithPendingTasks);
        assert_eq!(parent_clone.retained_stream_observations()[0].1.custody, RecoveryCustody::ReleasedWithPendingTasks);
        assert!(parent_journal.has_unresolved_tasks());
        assert!(!parent_journal.snapshot().is_complete());
    }
    #[tokio::test]
    async fn released_parent_admission_cannot_restart_stream_child_as_new_root() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("released-full-child").then(Effect),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(), &StreamRecoveryAdmission::new(1), None).unwrap();
        let admission = child.admit_to_parent(parent.client()).await.unwrap();
        admission.release();
        let report = child.initialize_and_drive(239, &Init, None, std::time::Duration::from_secs(5), |_| async {
            panic!("unregistered child must not start driver");
            #[allow(unreachable_code)] completed()
        }).await.unwrap();
        assert!(matches!(report, StreamInitializationReport::Rejected { prefix:
            crate::ExecutionReport::Unpersisted { result: crate::ExecutionTerminal::Outcome(Outcome::Emit(signal, _)), .. } }
                if signal == "execution.parent_custody_released"));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        assert!(!child.inner.context.lock().await.1.has::<crate::nested_custody::CustodyStore>());
    }
    #[tokio::test]
    async fn parent_stream_binding_preserves_policy_and_rejects_busy_started_and_duplicate() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let mut bus = Bus::new();
        bus.set_access_policy("deny-framework-fixture", Some(ranvier_core::BusAccessPolicy::allow_only(Vec::new())));
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("binding-policy").then(Effect),
            OwnedStreamContext::try_new(resources.clone(), bus).unwrap(), &StreamRecoveryAdmission::new(1), None).unwrap();
        let held = child.inner.context.lock().await;
        assert!(matches!(child.admit_to_parent(parent.client()).await, Err("execution.nested_stream_context_busy")));
        drop(held);
        let admission = child.admit_to_parent(parent.client()).await.unwrap();
        assert!(matches!(child.admit_to_parent(parent.client()).await, Err("execution.nested_stream_already_bound")));
        {
            let mut context = child.inner.context.lock().await;
            assert!(!context.1.has::<ParentStreamCustody>());
            let access = context.1.take_access_scope();
            assert!(context.1.has::<ParentStreamCustody>());
            assert!(context.1.has::<crate::nested_custody::WeakCustodyClient>());
            context.1.restore_access_scope(access);
            child.inner.started.store(true, Ordering::Release);
            assert!(matches!(child.admit_to_parent(parent.client()).await, Err("execution.nested_stream_already_started")));
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        admission.release();
    }
    #[tokio::test]
    async fn parent_store_retains_child_after_actual_initializer_abort_without_driver_poll() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let weak = Arc::downgrade(&resources);
        let mut bus = Bus::new();
        bus.insert(9911u64);
        let graph = Axon::<u32, u32, String, Resources>::start("parent-child-init-abort")
            .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo);
        let external_registry = graph.saga_compensation_registry.clone();
        let child = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), bus).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let admission = child.admit_to_parent(parent.client()).await.unwrap();
        // External graph mutation must not remove the actual frozen recovery handler.
        external_registry.write().unwrap().handlers.clear();
        drop(external_registry);
        let observer = child.observe();
        let entered = Arc::new(Notify::new());
        let signal = entered.clone();
        let drivers = Arc::new(AtomicUsize::new(0));
        let driver_calls = drivers.clone();
        let task = tokio::spawn(async move {
            child.initialize_and_drive(251, &PendingInit(signal), None,
                std::time::Duration::from_secs(30), move |_| async move {
                    driver_calls.fetch_add(1, Ordering::SeqCst);
                    completed()
                }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
        assert_eq!(observer.borrow().phase, SupervisorPhase::Initializing);
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
        assert_eq!(drivers.load(Ordering::SeqCst), 0);
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        let inner = admission.upgrade().unwrap()
            .downcast::<RecoveryInner<u32, u32, String, Resources>>()
            .unwrap_or_else(|_| panic!("wrong initializer recovery owner"));
        {
            let context = inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            assert_eq!(context.1.read::<u64>(), Some(&9911));
            let stack = context.1.read::<SagaStack>().unwrap();
            assert_eq!(stack.tasks.len(), 1);
            let retained = &stack.tasks[0];
            assert_eq!(retained.input_snapshot, b"251");
            assert!(inner.graph.saga_compensation_registry.read().unwrap().get(&retained.node_id).is_some());
        }
        let registry = Arc::downgrade(&inner.graph.saga_compensation_registry);
        drop(inner);
        drop(resources);
        assert!(weak.upgrade().is_some());
        assert!(registry.upgrade().is_some());
        drop(parent);
        assert!(weak.upgrade().is_none());
        assert!(registry.upgrade().is_none());
        assert_eq!(observer.borrow().custody, RecoveryCustody::ReleasedWithPendingTasks);
    }
    #[tokio::test]
    async fn parent_store_retains_actual_child_pending_undo_and_remaining_input_after_abort() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let weak = Arc::downgrade(&resources);
        let entered = Arc::new(Notify::new());
        let captured = Arc::new(std::sync::Mutex::new(None));
        let graph = Axon::<u32, u32, String, Resources>::start("parent-child-undo-abort")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(AdvancingEffect, Undo)
            .then_compensated(AdvancingEffect, PendingUndo(entered.clone(), captured.clone()))
            .then(StopPrefix(None));
        let child = OwnedStreamRecovery::try_new(graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let admission = child.admit_to_parent(parent.client()).await.unwrap();
        let observer = child.observe();
        let task = tokio::spawn(async move {
            child.initialize_and_drive(263, &Init, None, std::time::Duration::from_secs(30), |_| async {
                panic!("faulted prefix must not start driver");
                #[allow(unreachable_code)] completed()
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        let journal = captured.lock().unwrap().clone().unwrap();
        assert!(!journal.snapshot().is_complete());
        assert_eq!(journal.snapshot().steps[0].outcome, crate::SagaRollbackOutcome::InProgress);
        let unresolved = journal.unresolved_tasks();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].input_snapshot, b"264");
        assert_eq!(journal.snapshot().steps.len(), 1);
        let inner = admission.upgrade().unwrap()
            .downcast::<RecoveryInner<u32, u32, String, Resources>>()
            .unwrap_or_else(|_| panic!("wrong child cleanup owner"));
        {
            let context = inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            assert!(!context.1.has::<crate::nested_custody::CustodyStore>());
            assert!(context.1.has::<crate::nested_custody::WeakCustodyClient>());
            assert!(inner.graph.saga_compensation_registry.read().unwrap().get(&unresolved[0].node_id).is_some());
            let stack = context.1.read::<SagaStack>().unwrap();
            assert_eq!(stack.tasks.len(), 1);
            assert_eq!(stack.tasks[0].input_snapshot, b"263");
            assert!(inner.graph.saga_compensation_registry.read().unwrap().get(&stack.tasks[0].node_id).is_some());
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        drop(unresolved);
        let registry = Arc::downgrade(&inner.graph.saga_compensation_registry);
        drop(inner);
        drop(resources);
        assert!(weak.upgrade().is_some());
        drop(parent);
        assert!(weak.upgrade().is_none());
        assert!(registry.upgrade().is_none());
        assert_eq!(observer.borrow().custody, RecoveryCustody::ReleasedWithPendingTasks);
        assert_eq!(journal.snapshot().steps[0].outcome, crate::SagaRollbackOutcome::InProgress);
    }
    #[tokio::test]
    async fn parent_store_retains_child_when_actual_bounded_producer_aborts_at_full_queue() {
        struct Source(Arc<AtomicUsize>, Arc<AtomicUsize>);
        impl futures_core::Stream for Source {
            type Item = u32;
            fn poll_next(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<Option<u32>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                std::task::Poll::Ready(Some(277))
            }
        }
        impl Drop for Source {
            fn drop(&mut self) { self.1.fetch_add(1, Ordering::SeqCst); }
        }
        struct QueueInit(Arc<AtomicUsize>, Arc<AtomicUsize>);
        #[async_trait::async_trait]
        impl StreamingTransition<u32> for QueueInit {
            type Item = u32;
            type Error = String;
            type Resources = Resources;
            async fn run_stream(&self, _: u32, _: &Resources, _: &mut Bus) -> Result<RawStream<u32>, String> {
                Ok(Box::pin(Source(self.0.clone(), self.1.clone())))
            }
        }
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let weak = Arc::downgrade(&resources);
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("parent-full-queue")
                .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let admission = child.admit_to_parent(parent.client()).await.unwrap();
        let observer = child.observe();
        let polls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let initializer = QueueInit(polls.clone(), drops.clone());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let token = CancellationToken::new();
        let task = tokio::spawn(async move {
            child.initialize_and_drive(277, &initializer, None, std::time::Duration::from_secs(30), move |source| {
                produce_bounded(source, ranvier_core::streaming::StreamTimeoutConfig {
                    init: None, idle: None, total: None,
                }, token, sender)
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while receiver.len() != 1 { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(observer.borrow().phase, SupervisorPhase::Consuming);
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap(), Some(277));
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap(), None);
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        let inner = admission.upgrade().unwrap().downcast::<RecoveryInner<u32, u32, String, Resources>>()
            .unwrap_or_else(|_| panic!("wrong full-queue child owner"));
        {
            let context = inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            let stack = context.1.read::<SagaStack>().unwrap();
            assert_eq!(stack.tasks.len(), 1);
            assert_eq!(stack.tasks[0].input_snapshot, b"277");
            assert!(inner.graph.saga_compensation_registry.read().unwrap().get(&stack.tasks[0].node_id).is_some());
            assert!(context.1.has::<crate::nested_custody::WeakCustodyClient>());
            assert!(!context.1.has::<crate::nested_custody::CustodyStore>());
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        let registry = Arc::downgrade(&inner.graph.saga_compensation_registry);
        drop(inner);
        drop(resources);
        assert!(weak.upgrade().is_some());
        drop(parent);
        assert!(weak.upgrade().is_none());
        assert!(registry.upgrade().is_none());
        assert_eq!(observer.borrow().custody, RecoveryCustody::ReleasedWithPendingTasks);
    }
    #[tokio::test]
    async fn parent_store_retains_child_during_actual_prefix_effect_abort_before_initializer() {
        #[derive(Clone)]
        struct PendingPrefix(Arc<Notify>);
        #[async_trait::async_trait]
        impl Transition<u32, u32> for PendingPrefix {
            type Error = String;
            type Resources = Resources;
            async fn run(&self, _: u32, resources: &Resources, bus: &mut Bus) -> Outcome<u32, String> {
                resources.effects.fetch_add(1, Ordering::SeqCst);
                bus.insert(9922u64);
                self.0.notify_one();
                std::future::pending().await
            }
        }
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let weak = Arc::downgrade(&resources);
        let entered = Arc::new(Notify::new());
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("parent-prefix-abort")
                .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo)
                .then(PendingPrefix(entered.clone())),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let admission = child.admit_to_parent(parent.client()).await.unwrap();
        let observer = child.observe();
        let initialized = Arc::new(Notify::new());
        let init_signal = initialized.clone();
        let task = tokio::spawn(async move {
            child.initialize_and_drive(281, &PendingInit(init_signal), None, std::time::Duration::from_secs(30), |_| async {
                panic!("pending prefix must not start driver");
                #[allow(unreachable_code)] completed()
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        // The cancelled task is terminal: no remaining producer can signal initializer entry.
        assert!(futures_util::FutureExt::now_or_never(initialized.notified()).is_none());
        let inner = admission.upgrade().unwrap().downcast::<RecoveryInner<u32, u32, String, Resources>>()
            .unwrap_or_else(|_| panic!("wrong prefix child owner"));
        {
            let context = inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            assert_eq!(context.1.read::<u64>(), Some(&9922));
            assert!(context.1.has::<crate::nested_custody::WeakCustodyClient>());
            assert!(!context.1.has::<crate::nested_custody::CustodyStore>());
            let stack = context.1.read::<SagaStack>().unwrap();
            assert_eq!(stack.tasks.len(), 1);
            assert_eq!(stack.tasks[0].input_snapshot, b"281");
            assert!(inner.graph.saga_compensation_registry.read().unwrap().get(&stack.tasks[0].node_id).is_some());
        }
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        let registry = Arc::downgrade(&inner.graph.saga_compensation_registry);
        drop(inner);
        drop(resources);
        assert!(weak.upgrade().is_some());
        drop(parent);
        assert!(weak.upgrade().is_none());
        assert!(registry.upgrade().is_none());
        assert_eq!(observer.borrow().custody, RecoveryCustody::ReleasedWithPendingTasks);
    }
    #[tokio::test]
    async fn parent_stream_journal_rejects_admission_without_adding_phantom_rows() {
        for closed in [false, true] {
            let parent = crate::nested_custody::CustodyStore::new(0);
            let client = parent.client();
            let retained = if closed { drop(parent); None } else { Some(parent) };
            let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
            let child = OwnedStreamRecovery::try_new(
                Axon::<u32, u32, String, Resources>::start("rejected-parent-report").then(Effect),
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1), None).unwrap();
            let journal = crate::SagaRollbackJournal::new("parent-admission-failure");
            journal.finish();
            let before = journal.snapshot();
            assert!(matches!(child.admit_to_parent_journal(client, &journal).await,
                Err(signal) if signal == if closed { "execution.parent_custody_released" } else { "execution.nested_custody_capacity" }));
            assert_eq!(journal.snapshot(), before);
            assert!(journal.retained_stream_observations().is_empty());
            assert!(!journal.has_unresolved_tasks());
            assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
            let context = child.inner.context.lock().await;
            assert!(!context.1.has::<ParentStreamCustody>());
            assert!(!context.1.has::<crate::nested_custody::WeakCustodyClient>());
            drop(context);
            drop(retained);
        }
    }
    #[tokio::test]
    async fn parent_stream_journal_keeps_returned_successful_child_effects_unresolved() {
        struct EofInit;
        #[async_trait::async_trait]
        impl StreamingTransition<u32> for EofInit {
            type Item = u32;
            type Error = String;
            type Resources = Resources;
            async fn run_stream(&self, _: u32, _: &Resources, _: &mut Bus) -> Result<RawStream<u32>, String> {
                Ok(Box::pin(futures_util::stream::empty()))
            }
        }
        let parent = crate::nested_custody::CustodyStore::new(1);
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let weak = Arc::downgrade(&resources);
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("returned-child-report")
                .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let journal = crate::SagaRollbackJournal::new("parent-returned-child");
        let admission = child.admit_to_parent_journal(parent.client(), &journal).await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let report = child.initialize_and_drive(293, &EofInit, None, std::time::Duration::from_secs(5), move |source| {
            produce_bounded(source, ranvier_core::streaming::StreamTimeoutConfig {
                init: None, idle: None, total: None,
            }, CancellationToken::new(), sender)
        }).await.unwrap();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap(), None);
        assert!(matches!(report, StreamInitializationReport::Ready { .. }));
        let observations = journal.retained_stream_observations();
        assert_eq!(observations[0].1.phase, SupervisorPhase::Returned);
        assert_eq!(observations[0].1.disposition, Some(SupervisorDisposition::Consumption(crate::StreamConsumptionTerminal::Completed)));
        journal.finish();
        assert!(!journal.snapshot().is_complete());
        assert!(journal.has_unresolved_tasks());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        assert_eq!(child.inner.context.lock().await.1.read::<SagaStack>().unwrap().tasks[0].input_snapshot, b"293");
        drop(child);
        drop(resources);
        assert!(weak.upgrade().is_some());
        admission.release();
        assert!(weak.upgrade().is_none());
        assert!(journal.has_unresolved_tasks());
        assert!(!journal.snapshot().is_complete());
        assert_eq!(journal.retained_stream_observations()[0].1.custody, RecoveryCustody::ReleasedWithPendingTasks);
    }
    #[tokio::test]
    async fn bound_parent_control_cancels_actual_child_initializer_and_rejects_override_before_effects() {
        for override_control in [false, true] {
            let parent = crate::nested_custody::CustodyStore::new(1);
            let control = CancellationToken::new();
            let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
            let child = OwnedStreamRecovery::try_new(
                Axon::<u32, u32, String, Resources>::start("inherited-child-control")
                    .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo),
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1), None).unwrap();
            let journal = crate::SagaRollbackJournal::new("inherited-control-parent");
            let admission = child.admit_to_parent_journal_with_control(parent.client(), &journal, Some(control.clone())).await.unwrap();
            parent.close_admission().unwrap();
            assert!(matches!(parent.reserve(Arc::new(())), Err("execution.nested_custody_admission_closed")));
            let entered = Arc::new(Notify::new());
            let signal = entered.clone();
            let task = tokio::spawn(async move {
                child.initialize_and_drive(307, &PendingInit(signal),
                    override_control.then(CancellationToken::new), std::time::Duration::from_secs(5), |_| async {
                        panic!("cancelled or rejected initializer cannot run driver");
                        #[allow(unreachable_code)] completed()
                    }).await
            });
            if !override_control {
                tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
                control.cancel(ranvier_core::CancellationReason::OperatorShutdown);
            }
            assert_eq!(journal.drain_stream_children(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await, Ok(()));
            let report = tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
            if override_control {
                assert!(matches!(report, StreamInitializationReport::Rejected { prefix:
                    crate::ExecutionReport::Unpersisted { result: crate::ExecutionTerminal::Outcome(Outcome::Emit(signal, _)), .. } }
                        if signal == "execution.nested_stream_control_override"));
                assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
                assert!(!control.is_cancelled());
            } else {
                assert!(matches!(report, StreamInitializationReport::Cancelled { context, .. }
                    if context.reason == ranvier_core::CancellationReason::OperatorShutdown));
                assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
                assert_eq!(control.context().unwrap().reason, ranvier_core::CancellationReason::OperatorShutdown);
                assert_eq!(journal.retained_stream_observations()[0].1.disposition, Some(SupervisorDisposition::InitializationCancelled));
            }
            admission.release();
        }
    }
    #[tokio::test]
    async fn bound_parent_control_precancel_and_local_cancel_reject_effects_without_upward_propagation() {
        for cancel_parent in [false, true] {
            let parent = crate::nested_custody::CustodyStore::new(1);
            let control = CancellationToken::new();
            let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
            let child = OwnedStreamRecovery::try_new(
                Axon::<u32, u32, String, Resources>::start("bound-control-before-effect").then(Effect),
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1), None).unwrap();
            let journal = crate::SagaRollbackJournal::new("precancel-parent-observation");
            let admission = child.admit_to_parent_journal_with_control(parent.client(), &journal, Some(control.clone())).await.unwrap();
            if cancel_parent {
                control.cancel(ranvier_core::CancellationReason::OperatorShutdown);
            } else {
                let context = child.inner.context.lock().await;
                context.1.read::<ParentStreamCustody>().unwrap().1.as_ref().unwrap()
                    .cancel(ranvier_core::CancellationReason::Explicit);
            }
            let report = child.initialize_and_drive(311, &Init, None, std::time::Duration::from_secs(5), |_| async {
                panic!("pre-cancelled bound child must not start driver");
                #[allow(unreachable_code)] completed()
            }).await.unwrap();
            assert!(matches!(report, StreamInitializationReport::Rejected { prefix:
                crate::ExecutionReport::Unpersisted { result: crate::ExecutionTerminal::Cancelled(context), .. } }
                    if context.reason == if cancel_parent { ranvier_core::CancellationReason::OperatorShutdown }
                        else { ranvier_core::CancellationReason::Explicit }));
            assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
            assert_eq!(control.is_cancelled(), cancel_parent);
            if !cancel_parent { assert!(control.context().is_none()); }
            let observed = journal.retained_stream_observations();
            assert_eq!(observed[0].1.phase, SupervisorPhase::Returned);
            assert_eq!(observed[0].1.disposition, Some(SupervisorDisposition::PrefixRejected));
            assert!(journal.has_unresolved_tasks());
            assert!(!journal.snapshot().is_complete());
            admission.release();
        }
    }
    #[tokio::test(start_paused = true)]
    async fn bound_parent_deadline_interrupts_actual_initializer_without_extending_original_deadline() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let root = CancellationToken::new();
        let control = root.child_with_deadline(std::time::Duration::from_secs(60));
        let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("inherited-parent-deadline")
                .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo),
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let journal = crate::SagaRollbackJournal::new("deadline-parent-observation");
        let admission = child.admit_to_parent_journal_with_control(parent.client(), &journal, Some(control.clone())).await.unwrap();
        let entered = Arc::new(Notify::new());
        let signal = entered.clone();
        let task = tokio::spawn(async move {
            child.initialize_and_drive(313, &PendingInit(signal), None, std::time::Duration::from_secs(5), |_| async {
                panic!("deadline-cancelled initializer must not start driver");
                #[allow(unreachable_code)] completed()
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        let report = tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap().unwrap();
        let StreamInitializationReport::Cancelled { context, .. } = report else { panic!("expected inherited deadline cancellation"); };
        assert_eq!(context.reason, ranvier_core::CancellationReason::DeadlineExceeded);
        assert_eq!(context, control.context().unwrap());
        assert!(context.deadline_ms.is_some());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        assert!(!root.is_cancelled());
        assert_eq!(journal.retained_stream_observations()[0].1.disposition, Some(SupervisorDisposition::InitializationCancelled));
        admission.release();
    }
    #[tokio::test(start_paused = true)]
    async fn stream_observation_drain_times_out_without_cancelling_or_releasing_child() {
        let parent = crate::nested_custody::CustodyStore::new(1);
        let control = CancellationToken::new();
        let child = OwnedStreamRecovery::try_new(
            Axon::<u32, u32, String, Resources>::start("drain-unstarted"),
            OwnedStreamContext::try_new(Arc::new(Resources { effects: AtomicUsize::new(0) }), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1), None).unwrap();
        let journal = crate::SagaRollbackJournal::new("drain-timeout");
        let admission = child.admit_to_parent_journal_with_control(parent.client(), &journal, Some(control.clone())).await.unwrap();
        assert_eq!(journal.drain_stream_children(tokio::time::Instant::now() + std::time::Duration::from_secs(3)).await, Err(1));
        assert!(!control.is_cancelled());
        assert!(!child.inner.started.load(Ordering::Acquire));
        assert!(admission.upgrade().is_some());
        drop(child);
        admission.release();
        assert_eq!(journal.drain_stream_children(tokio::time::Instant::now()).await, Ok(()));
        assert!(journal.has_unresolved_tasks());
        assert!(!journal.snapshot().is_complete());
    }
    #[tokio::test]
    async fn managed_task_preserves_actual_child_initialization_report_and_rejected_start_has_no_effects() {
        for grant_start in [false, true] {
            let parent = crate::nested_custody::CustodyStore::new(1);
            let control = CancellationToken::new();
            let resources = Arc::new(Resources { effects: AtomicUsize::new(0) });
            let weak = Arc::downgrade(&resources);
            let child = OwnedStreamRecovery::try_new(
                Axon::<u32, u32, String, Resources>::start("managed-actual-child")
                    .with_saga_policy(SagaPolicy::Enabled).then_compensated(Effect, Undo),
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1), None).unwrap();
            let entered = Arc::new(Notify::new());
            let signal = entered.clone();
            let task_child = child.clone();
            let (task, permit) = crate::managed_stream_task::ManagedStreamTask::prepare(async move {
                task_child.initialize_and_drive(331, &PendingInit(signal), None,
                    std::time::Duration::from_secs(5), |_| async {
                        panic!("cancelled initializer must not run driver");
                        #[allow(unreachable_code)] completed()
                    }).await
            });
            let task = Arc::new(Mutex::new(task));
            let journal = crate::SagaRollbackJournal::new("managed-actual-parent");
            let admission = child.admit_managed_to_parent(parent.client(), &journal, control.clone(), task.clone()).await.unwrap();
            let task_weak = Arc::downgrade(&task);
            drop(task);
            let task = task_weak.upgrade().expect("parent must own actual task holder");
            assert!(matches!(parent.closed_tasks(), Err("execution.nested_custody_admission_open")));
            parent.close_admission().unwrap();
            let registered_tasks = parent.closed_tasks().unwrap();
            assert_eq!(registered_tasks.len(), 1);
            assert!(Arc::ptr_eq(&registered_tasks[0].1, &(task.clone() as crate::nested_custody::Capability)));
            drop(registered_tasks);
            assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
            assert!(!child.inner.started.load(Ordering::Acquire));
            if grant_start {
                permit.send(()).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified()).await.unwrap();
                control.cancel(ranvier_core::CancellationReason::OperatorShutdown);
            } else { drop(permit); }
            type ChildReport = Result<StreamInitializationReport<u32, StreamReadyResult<()>, String>, StreamInvocationAlreadyStarted>;
            let settled = parent.settle_closed_tasks::<ChildReport>(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await.unwrap();
            assert_eq!(settled.len(), 1);
            assert_eq!(settled[0].1, Ok(()));
            let joined = task.lock().await.take_joined().unwrap().unwrap();
            if grant_start {
                assert!(matches!(joined, Some(Ok(StreamInitializationReport::Cancelled { context, .. }))
                    if context.reason == ranvier_core::CancellationReason::OperatorShutdown));
                assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
                assert_eq!(journal.retained_stream_observations()[0].1.disposition, Some(SupervisorDisposition::InitializationCancelled));
            } else {
                assert!(joined.is_none());
                assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
                assert!(!child.inner.started.load(Ordering::Acquire));
            }
            drop(task);
            drop(child);
            drop(resources);
            assert!(weak.upgrade().is_some());
            admission.release();
            assert!(weak.upgrade().is_none());
            assert!(task_weak.upgrade().is_some());
            drop(settled);
            assert!(task_weak.upgrade().is_none());
        }
    }
    struct ExternalHook {
        mode: u8,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl crate::persistence::CompensationHook for ExternalHook {
        async fn compensate(
            &self,
            _: crate::persistence::CompensationContext,
        ) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                1 => std::future::pending().await,
                4 => Err(anyhow::anyhow!("private hook failure")),
                _ => Ok(()),
            }
        }
    }
    struct ExternalMarker {
        mode: u8,
        applied: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl crate::persistence::CompensationIdempotencyStore for ExternalMarker {
        async fn was_compensated(&self, _: &str) -> anyhow::Result<bool> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.mode == 6 {
                return Err(anyhow::anyhow!("private read failure"));
            }
            if self.mode == 0 {
                std::future::pending().await
            } else {
                Ok(self.applied.load(Ordering::SeqCst))
            }
        }
        async fn mark_compensated(&self, _: &str) -> anyhow::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.mode == 2 {
                return std::future::pending().await;
            }
            self.applied.store(true, Ordering::SeqCst);
            if self.mode == 5 {
                return Err(anyhow::anyhow!("private applied marker failure"));
            }
            if self.mode == 3 {
                std::future::pending().await
            } else {
                Ok(())
            }
        }
    }
    #[tokio::test]
    async fn external_read_hook_marker_and_backoff_share_grace_and_retain_exact_capability() {
        use crate::persistence::{
            CompensationHandle, CompensationIdempotencyHandle, CompensationRetryPolicy,
            InMemoryPersistenceStore, PersistenceHandle, PersistenceStore, PersistenceTraceId,
        };
        for (mode, grace) in (0..7)
            .map(|mode| (mode, std::time::Duration::from_millis(100)))
            .chain([(0, std::time::Duration::ZERO)])
        {
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let hook: Arc<dyn crate::persistence::CompensationHook> = Arc::new(ExternalHook {
                mode,
                calls: calls.clone(),
            });
            let applied = Arc::new(AtomicBool::new(false));
            let reads = Arc::new(AtomicUsize::new(0));
            let writes = Arc::new(AtomicUsize::new(0));
            let marker: Arc<dyn crate::persistence::CompensationIdempotencyStore> =
                Arc::new(ExternalMarker {
                    mode,
                    applied: applied.clone(),
                    reads: reads.clone(),
                    writes: writes.clone(),
                });
            let store = Arc::new(InMemoryPersistenceStore::new());
            let mut bus = Bus::new();
            bus.insert(PersistenceHandle::from_arc(store.clone()));
            bus.insert(PersistenceTraceId::new("external-grace-trace"));
            bus.insert(CompensationHandle::from_arc(hook.clone()));
            bus.insert(CompensationIdempotencyHandle::from_arc(marker));
            bus.insert(CompensationRetryPolicy {
                max_attempts: 3,
                backoff_ms: 1000,
            });
            let graph = Axon::<u32, u32, String, Resources>::start("private-external-circuit")
                .then(FailingEffect);
            let owner = OwnedStreamRecovery::try_new(
                graph,
                OwnedStreamContext::try_new(resources.clone(), bus).unwrap(),
                &StreamRecoveryAdmission::new(1),
                None,
            )
            .unwrap();
            let report = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                owner.initialize_and_drive(127, &Init, None, grace, |_| async { completed() }),
            )
            .await
            .unwrap()
            .unwrap();
            let StreamInitializationReport::Rejected { prefix } = report else {
                panic!("expected external cleanup prefix rejection");
            };
            let cleanup = prefix.cleanup().unwrap();
            assert_eq!(cleanup.finished, mode >= 5);
            assert!(!cleanup.is_complete());
            assert_eq!(cleanup.steps.len(), 1);
            assert_eq!(
                cleanup.steps[0].outcome,
                if mode == 0 || mode == 6 {
                    crate::SagaRollbackOutcome::NotInvoked
                } else if mode == 5 {
                    crate::SagaRollbackOutcome::Fault
                } else {
                    crate::SagaRollbackOutcome::InProgress
                }
            );
            assert!(
                !serde_json::to_string(cleanup)
                    .unwrap()
                    .contains("private-external-circuit")
            );
            match prefix {
                crate::ExecutionReport::Recorded { result, .. }
                | crate::ExecutionReport::Unconfirmed { result, .. } => assert!(
                    matches!(result, crate::ExecutionTerminal::Outcome(Outcome::Fault(error)) if error == "original direct fault")
                ),
                _ => panic!("original terminal missing"),
            }
            let context = owner.inner.context.lock().await;
            assert!(Arc::ptr_eq(&context.0, &resources));
            let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
            let retained = journal.unresolved_external();
            assert_eq!(retained.len(), 1);
            assert_eq!(retained[0].context.trace_id, "external-grace-trace");
            assert_eq!(retained[0].context.circuit, "private-external-circuit");
            assert_eq!(retained[0].context.fault_kind, "Fault");
            assert!(Arc::ptr_eq(&retained[0].handle.hook(), &hook));
            assert!(retained[0].idempotency.is_some());
            assert_eq!(
                retained[0].invoked.load(Ordering::SeqCst),
                mode != 0 && mode != 6
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::from(mode != 0 && mode != 6)
            );
            assert_eq!(
                reads.load(Ordering::SeqCst),
                usize::from(grace != std::time::Duration::ZERO)
            );
            assert_eq!(
                writes.load(Ordering::SeqCst),
                usize::from(mode == 2 || mode == 3 || mode == 5)
            );
            assert_eq!(applied.load(Ordering::SeqCst), mode == 3 || mode == 5);
            assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
            assert_eq!(owner.observe().borrow().cleanup_timed_out, mode < 5);
            assert_eq!(owner.observe().borrow().cleanup_complete, Some(false));
            assert!(!format!("{journal:?}").contains("private-external-circuit"));
            drop(retained);
            drop(context);
            let persisted = store.load("external-grace-trace").await.unwrap().unwrap();
            assert!(
                !persisted
                    .events
                    .iter()
                    .any(|event| event.outcome_kind == "Compensated")
            );
            assert_eq!(
                persisted.completion,
                Some(crate::persistence::CompletionState::Fault)
            );
            let observer = owner.observe();
            drop(owner);
            assert_eq!(
                observer.borrow().custody,
                RecoveryCustody::ReleasedWithPendingTasks
            );
        }
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
    async fn direct_undo_timeout_retains_failed_step_input_and_handler_for_both_saga_policies() {
        for enabled in [false, true] {
            for grace in [
                std::time::Duration::ZERO,
                std::time::Duration::from_millis(50),
            ] {
                let entered = Arc::new(Notify::new());
                let captured = Arc::new(std::sync::Mutex::new(None));
                let resources = Arc::new(Resources {
                    effects: AtomicUsize::new(0),
                });
                let graph = Axon::<u32, u32, String, Resources>::start("direct-grace")
                    .with_saga_policy(if enabled {
                        SagaPolicy::Enabled
                    } else {
                        SagaPolicy::Disabled
                    })
                    .then_compensated(AdvancingEffect, Undo)
                    .then_compensated(FailingEffect, PendingUndo(entered, captured.clone()));
                let owner = OwnedStreamRecovery::try_new(
                    graph,
                    OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                    &StreamRecoveryAdmission::new(1),
                    None,
                )
                .unwrap();
                let report = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    owner.initialize_and_drive(71, &Init, None, grace, |_| async {
                        panic!("failed prefix must not start driver");
                        #[allow(unreachable_code)]
                        completed()
                    }),
                )
                .await
                .unwrap()
                .unwrap();
                let StreamInitializationReport::Rejected { prefix } = report else {
                    panic!("expected rejected prefix");
                };
                let cleanup = prefix.cleanup().unwrap().clone();
                assert!(!cleanup.finished);
                assert!(!cleanup.is_complete());
                assert_eq!(cleanup.steps.len(), 1);
                assert_eq!(
                    cleanup.steps[0].outcome,
                    if grace == std::time::Duration::ZERO {
                        crate::SagaRollbackOutcome::NotInvoked
                    } else {
                        crate::SagaRollbackOutcome::InProgress
                    }
                );
                assert!(matches!(prefix, crate::ExecutionReport::Unpersisted {
                    result: crate::ExecutionTerminal::Outcome(Outcome::Fault(error)), ..
                } if error == "original direct fault"));
                let context = owner.inner.context.try_lock().unwrap();
                let retained = context
                    .1
                    .read::<crate::SagaRollbackJournal>()
                    .unwrap()
                    .unresolved_tasks();
                assert_eq!(retained.len(), 1);
                assert_eq!(retained[0].input_snapshot, b"72");
                assert_eq!(retained[0].node_id, cleanup.steps[0].node_id);
                assert!(
                    owner
                        .inner
                        .graph
                        .saga_compensation_registry
                        .read()
                        .unwrap()
                        .get(&retained[0].node_id)
                        .is_some()
                );
                assert_eq!(
                    context
                        .1
                        .read::<SagaStack>()
                        .map(|stack| stack.tasks.len())
                        .unwrap_or(0),
                    usize::from(enabled)
                );
                assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
                assert_eq!(
                    captured.lock().unwrap().is_some(),
                    grace != std::time::Duration::ZERO
                );
                assert_eq!(owner.observe().borrow().cleanup_complete, Some(false));
                assert!(owner.observe().borrow().cleanup_timed_out);
                assert_eq!(owner.observe().borrow().phase, SupervisorPhase::Returned);
                assert!(
                    !serde_json::to_string(&cleanup)
                        .unwrap()
                        .contains("input_snapshot")
                );
                drop(context);
                let observer = owner.observe();
                drop(owner);
                assert_eq!(
                    observer.borrow().custody,
                    RecoveryCustody::ReleasedWithPendingTasks
                );
            }
        }
    }
    #[tokio::test]
    async fn completed_direct_undo_releases_only_its_retained_input_and_preserves_fault() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("completed-direct")
            .then_compensated(FailingEffect, Undo);
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let report = owner
            .initialize_and_drive(
                113,
                &Init,
                None,
                std::time::Duration::from_secs(1),
                |_| async { completed() },
            )
            .await
            .unwrap();
        let StreamInitializationReport::Rejected { prefix } = report else {
            panic!("expected original fault");
        };
        assert!(prefix.cleanup().unwrap().is_complete());
        assert!(
            matches!(prefix, crate::ExecutionReport::Unpersisted { result: crate::ExecutionTerminal::Outcome(Outcome::Fault(error)), .. } if error == "original direct fault")
        );
        assert!(
            owner
                .inner
                .context
                .lock()
                .await
                .1
                .read::<crate::SagaRollbackJournal>()
                .unwrap()
                .unresolved_tasks()
                .is_empty()
        );
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        let observer = owner.observe();
        assert!(!observer.borrow().cleanup_timed_out);
        drop(owner);
        assert_eq!(
            observer.borrow().custody,
            RecoveryCustody::ReleasedWithoutRetainedTasks
        );
    }
    #[derive(Clone)]
    struct StopPrefix(Option<CancellationToken>);
    #[async_trait::async_trait]
    impl Transition<u32, u32> for StopPrefix {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, _: u32, _: &Resources, _: &mut Bus) -> Outcome<u32, String> {
            if let Some(token) = &self.0 {
                token.cancel(ranvier_core::CancellationReason::ClientDisconnected);
                std::future::pending().await
            } else {
                Outcome::Fault("original prefix fault".into())
            }
        }
    }
    #[tokio::test]
    async fn prefix_fault_and_cancel_share_bounded_cleanup_without_losing_original_terminal() {
        for cancellation in [false, true] {
            for pending_top in [false, true] {
                let entered = Arc::new(Notify::new());
                let captured = Arc::new(std::sync::Mutex::new(None));
                let resources = Arc::new(Resources {
                    effects: AtomicUsize::new(0),
                });
                let token = CancellationToken::new();
                let prefix = Axon::<u32, u32, String, Resources>::start("prefix-grace")
                    .with_saga_policy(SagaPolicy::Enabled);
                let pending = PendingUndo(entered.clone(), captured.clone());
                let graph = if pending_top {
                    prefix
                        .then_compensated(AdvancingEffect, Undo)
                        .then_compensated(AdvancingEffect, pending)
                } else {
                    prefix
                        .then_compensated(AdvancingEffect, pending)
                        .then_compensated(AdvancingEffect, Undo)
                }
                .then(StopPrefix(cancellation.then_some(token.clone())));
                let owner = OwnedStreamRecovery::try_new(
                    graph,
                    OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                    &StreamRecoveryAdmission::new(1),
                    None,
                )
                .unwrap();
                let task_owner = owner.clone();
                let task = tokio::spawn(async move {
                    task_owner
                        .initialize_and_drive(
                            97,
                            &Init,
                            Some(token),
                            std::time::Duration::from_millis(100),
                            |_| async {
                                panic!("rejected prefix must not start driver");
                                #[allow(unreachable_code)]
                                completed()
                            },
                        )
                        .await
                });
                tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
                    .await
                    .unwrap();
                assert_eq!(owner.observe().borrow().phase, SupervisorPhase::CleaningUp);
                let report = tokio::time::timeout(std::time::Duration::from_secs(3), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let StreamInitializationReport::Rejected { prefix } = report else {
                    panic!("expected prefix rejection");
                };
                let cleanup = prefix.cleanup().unwrap();
                assert!(!cleanup.finished);
                assert!(!cleanup.is_complete());
                assert_eq!(cleanup.steps.len(), if pending_top { 1 } else { 2 });
                assert_eq!(
                    cleanup.steps.last().unwrap().outcome,
                    crate::SagaRollbackOutcome::InProgress
                );
                match prefix {
                    crate::ExecutionReport::Unpersisted {
                        result: crate::ExecutionTerminal::Cancelled(context),
                        ..
                    } if cancellation => assert_eq!(
                        context.reason,
                        ranvier_core::CancellationReason::ClientDisconnected
                    ),
                    crate::ExecutionReport::Unpersisted {
                        result: crate::ExecutionTerminal::Outcome(Outcome::Fault(error)),
                        ..
                    } if !cancellation => assert_eq!(error, "original prefix fault"),
                    _ => panic!("original terminal was lost"),
                }
                let context = owner.inner.context.try_lock().unwrap();
                assert!(Arc::ptr_eq(&context.0, &resources));
                let retained = context
                    .1
                    .read::<crate::SagaRollbackJournal>()
                    .unwrap()
                    .unresolved_tasks();
                assert_eq!(retained.len(), 1);
                assert_eq!(
                    retained[0].input_snapshot,
                    if pending_top { b"98" } else { b"97" }
                );
                assert_eq!(
                    context.1.read::<SagaStack>().unwrap().tasks.len(),
                    usize::from(pending_top)
                );
                assert_eq!(
                    resources.effects.load(Ordering::SeqCst),
                    if pending_top { 2 } else { 3 }
                );
                assert!(owner.observe().borrow().cleanup_timed_out);
                assert_eq!(owner.observe().borrow().cleanup_complete, Some(false));
                assert_eq!(
                    owner.observe().borrow().disposition,
                    Some(SupervisorDisposition::PrefixRejected)
                );
                drop(context);
            }
        }
    }
    struct GraceInit(u8, CancellationToken);
    #[async_trait::async_trait]
    impl StreamingTransition<u32> for GraceInit {
        type Item = u32;
        type Error = String;
        type Resources = Resources;
        async fn run_stream(
            &self,
            _: u32,
            _: &Resources,
            _: &mut Bus,
        ) -> Result<RawStream<u32>, String> {
            match self.0 {
                1 => Err("fixture initializer failure".into()),
                2 => {
                    self.1
                        .cancel(ranvier_core::CancellationReason::ClientDisconnected);
                    std::future::pending().await
                }
                _ => Ok(Box::pin(futures_util::stream::pending())),
            }
        }
    }
    #[tokio::test]
    async fn cleanup_grace_returns_partial_report_and_retains_live_reconciliation_context() {
        for grace in [
            std::time::Duration::ZERO,
            std::time::Duration::from_millis(50),
            std::time::Duration::MAX,
        ] {
            for mode in 0..3 {
                let entered = Arc::new(Notify::new());
                let observed_journal = Arc::new(std::sync::Mutex::new(None));
                let resources = Arc::new(Resources {
                    effects: AtomicUsize::new(0),
                });
                let graph = Axon::<u32, u32, String, Resources>::start("cleanup-grace")
                    .with_saga_policy(SagaPolicy::Enabled)
                    .then_compensated(AdvancingEffect, Undo)
                    .then_compensated(
                        AdvancingEffect,
                        PendingUndo(entered.clone(), observed_journal.clone()),
                    );
                let admission = StreamRecoveryAdmission::new(1);
                let owner = OwnedStreamRecovery::try_new(
                    graph,
                    OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                    &admission,
                    None,
                )
                .unwrap();
                let token = CancellationToken::new();
                let drivers = Arc::new(AtomicUsize::new(0));
                let driver_calls = drivers.clone();
                let report = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    owner.initialize_and_drive(
                        89,
                        &GraceInit(mode, token.clone()),
                        Some(token),
                        grace,
                        move |source| async move {
                            driver_calls.fetch_add(1, Ordering::SeqCst);
                            let consumed = crate::ObservedStream::new(
                                source,
                                ranvier_core::streaming::StreamTimeoutConfig {
                                    init: None,
                                    idle: None,
                                    total: Some(std::time::Duration::ZERO),
                                },
                                None,
                            )
                            .collect_report()
                            .await;
                            StreamDriveResult {
                                value: consumed.items,
                                terminal: consumed.terminal,
                            }
                        },
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                let cleanup = match report {
                    StreamInitializationReport::Ready { value, .. } if mode == 0 => {
                        assert_eq!(
                            value.consumption.unwrap().terminal,
                            crate::StreamConsumptionTerminal::TimedOut(
                                crate::StreamTimeoutKind::Total
                            )
                        );
                        value.cleanup.unwrap()
                    }
                    StreamInitializationReport::Failed {
                        initialization_cleanup,
                        ..
                    } if mode == 1 => initialization_cleanup.unwrap(),
                    StreamInitializationReport::Cancelled {
                        initialization_cleanup,
                        ..
                    } if mode == 2 => initialization_cleanup.unwrap(),
                    _ => panic!("wrong initialization disposition"),
                };
                assert!(!cleanup.finished);
                assert!(!cleanup.is_complete());
                let exhausted_at_entry = grace != std::time::Duration::from_millis(50);
                assert_eq!(cleanup.steps.len(), usize::from(!exhausted_at_entry));
                if !exhausted_at_entry {
                    assert_eq!(
                        cleanup.steps[0].outcome,
                        crate::SagaRollbackOutcome::InProgress
                    );
                }
                assert_eq!(drivers.load(Ordering::SeqCst), usize::from(mode == 0));
                assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
                if !exhausted_at_entry {
                    assert_eq!(
                        observed_journal
                            .lock()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .snapshot(),
                        cleanup
                    );
                }
                let context = owner
                    .inner
                    .context
                    .try_lock()
                    .expect("cleanup returned ownership");
                assert!(Arc::ptr_eq(&resources, &context.0));
                let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
                if exhausted_at_entry {
                    assert!(journal.unresolved_tasks().is_empty());
                    assert_eq!(context.1.read::<SagaStack>().unwrap().tasks.len(), 2);
                } else {
                    assert_eq!(journal.unresolved_tasks()[0].input_snapshot, b"90");
                    assert_eq!(
                        context.1.read::<SagaStack>().unwrap().tasks[0].input_snapshot,
                        b"89"
                    );
                }
                drop(context);
                assert_eq!(owner.observe().borrow().phase, SupervisorPhase::Returned);
                assert_eq!(owner.observe().borrow().cleanup_complete, Some(false));
                assert!(owner.observe().borrow().cleanup_timed_out);
                assert_eq!(owner.observe().borrow().custody, RecoveryCustody::Held);
                assert_eq!(admission.in_use().unwrap(), 1);
                assert!(matches!(
                    owner
                        .initialize_and_drive(
                            99,
                            &Init,
                            None,
                            std::time::Duration::from_secs(1),
                            |_| async { completed() }
                        )
                        .await,
                    Err(StreamInvocationAlreadyStarted)
                ));
                drop(owner);
                assert_eq!(admission.in_use().unwrap(), 0);
            }
        }
    }
    #[tokio::test]
    async fn forced_abort_during_compensation_retains_popped_and_remaining_inputs() {
        for pending_top in [true, false] {
            let entered = Arc::new(Notify::new());
            let observed_journal = Arc::new(std::sync::Mutex::new(None));
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let prefix = Axon::<u32, u32, String, Resources>::start("compensator-abort")
                .with_saga_policy(SagaPolicy::Enabled);
            let pending = PendingUndo(entered.clone(), observed_journal.clone());
            let graph = if pending_top {
                prefix
                    .then_compensated(AdvancingEffect, Undo)
                    .then_compensated(AdvancingEffect, pending)
            } else {
                prefix
                    .then_compensated(AdvancingEffect, pending)
                    .then_compensated(AdvancingEffect, Undo)
            };
            let owner = OwnedStreamRecovery::try_new(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1),
                None,
            )
            .unwrap();
            let registry = Arc::downgrade(&owner.inner.graph.saga_compensation_registry);
            let mut observer = owner.observe();
            let task_owner = owner.clone();
            let task = tokio::spawn(async move {
                task_owner
                    .initialize_and_drive(
                        53,
                        &Init,
                        None,
                        std::time::Duration::from_secs(5),
                        |source| async move {
                            let observed = crate::ObservedStream::new(
                                source,
                                ranvier_core::streaming::StreamTimeoutConfig {
                                    init: None,
                                    idle: None,
                                    total: Some(std::time::Duration::ZERO),
                                },
                                None,
                            )
                            .collect_report()
                            .await;
                            StreamDriveResult {
                                value: observed.items,
                                terminal: observed.terminal,
                            }
                        },
                    )
                    .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
                .await
                .unwrap();
            let live = observed_journal.lock().unwrap().clone().unwrap();
            let before_abort = live.snapshot();
            assert!(!before_abort.finished);
            assert_eq!(
                before_abort.steps.last().unwrap().outcome,
                crate::SagaRollbackOutcome::InProgress
            );
            assert!(owner.inner.context.try_lock().is_err());
            assert_eq!(
                observer.borrow_and_update().phase,
                SupervisorPhase::CleaningUp
            );
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            tokio::time::timeout(std::time::Duration::from_secs(1), observer.changed())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
            assert_eq!(observer.borrow().cleanup_complete, None);
            let context = owner.inner.context.lock().await;
            assert!(Arc::ptr_eq(&resources, &context.0));
            assert_eq!(
                resources.effects.load(Ordering::SeqCst),
                if pending_top { 2 } else { 3 }
            );
            let journal = context.1.read::<crate::SagaRollbackJournal>().unwrap();
            let report = journal.snapshot();
            assert!(!report.finished);
            assert!(!report.is_complete());
            assert_eq!(report.steps.len(), if pending_top { 1 } else { 2 });
            assert_eq!(live.snapshot(), report);
            if !pending_top {
                assert_eq!(
                    report.steps[0].outcome,
                    crate::SagaRollbackOutcome::Completed
                );
            }
            assert_eq!(
                report.steps.last().unwrap().outcome,
                crate::SagaRollbackOutcome::InProgress
            );
            let popped = journal.unresolved_tasks();
            assert_eq!(popped.len(), 1);
            assert_eq!(
                popped[0].input_snapshot,
                if pending_top { b"54" } else { b"53" }
            );
            assert_eq!(popped[0].node_id, report.steps.last().unwrap().node_id);
            let remaining = &context.1.read::<SagaStack>().unwrap().tasks;
            assert_eq!(remaining.len(), usize::from(pending_top));
            if pending_top {
                assert_eq!(remaining[0].input_snapshot, b"53");
                assert_ne!(remaining[0].node_id, popped[0].node_id);
            }
            let registry_owner = registry.upgrade().unwrap();
            for task in popped.iter().chain(remaining) {
                assert!(registry_owner.read().unwrap().get(&task.node_id).is_some());
            }
            assert!(!format!("{journal:?}").contains("input_snapshot"));
            drop(registry_owner);
            drop(context);
            assert!(matches!(
                owner
                    .initialize_and_drive(
                        99,
                        &Init,
                        None,
                        std::time::Duration::from_secs(5),
                        |_| async { completed() }
                    )
                    .await,
                Err(StreamInvocationAlreadyStarted)
            ));
            assert_eq!(
                resources.effects.load(Ordering::SeqCst),
                if pending_top { 2 } else { 3 }
            );
            assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
            drop(owner);
            assert_eq!(
                observer.borrow().custody,
                RecoveryCustody::ReleasedWithPendingTasks
            );
            assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
            assert_eq!(observer.borrow().cleanup_complete, None);
            assert!(registry.upgrade().is_none());
            assert_eq!(Arc::strong_count(&resources), 1);
        }
    }
    #[tokio::test]
    async fn late_observer_retains_local_completion_and_rejected_retry_cannot_rewrite_it() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("late-observer")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let report = owner
            .initialize_and_drive(
                61,
                &EndInit(true),
                None,
                std::time::Duration::from_secs(5),
                |source| async move {
                    let observed = crate::ObservedStream::new(
                        source,
                        ranvier_core::streaming::StreamTimeoutConfig {
                            init: None,
                            idle: None,
                            total: None,
                        },
                        None,
                    )
                    .collect_report()
                    .await;
                    StreamDriveResult {
                        value: observed.items,
                        terminal: observed.terminal,
                    }
                },
            )
            .await;
        assert!(matches!(
            report,
            Ok(StreamInitializationReport::Ready { .. })
        ));
        // No receiver existed while updates were published.
        let observer = owner.observe();
        let before = observer.borrow().clone();
        assert_eq!(before.phase, SupervisorPhase::Returned);
        assert_eq!(
            before.disposition,
            Some(SupervisorDisposition::Consumption(
                crate::StreamConsumptionTerminal::Completed
            ))
        );
        assert_eq!(before.cleanup_complete, None);
        assert_eq!(
            owner
                .inner
                .context
                .lock()
                .await
                .1
                .read::<SagaStack>()
                .unwrap()
                .tasks
                .len(),
            1
        );
        assert!(matches!(
            owner
                .initialize_and_drive(
                    62,
                    &Init,
                    None,
                    std::time::Duration::from_secs(5),
                    |_| async { completed() }
                )
                .await,
            Err(StreamInvocationAlreadyStarted)
        ));
        assert_eq!(*observer.borrow(), before);
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        assert!(!format!("{before:?}").contains("input_snapshot"));
        {
            let mut context = owner.inner.context.lock().await;
            context.1.set_access_policy(
                "custody-fixture",
                Some(ranvier_core::BusAccessPolicy::allow_only(vec![
                    ranvier_core::BusTypeRef::of::<u32>(),
                ])),
            );
            assert!(context.1.get::<SagaStack>().is_err());
        }
        let remaining_owner = owner.clone();
        assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
        drop(owner);
        assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
        drop(remaining_owner);
        assert_eq!(
            observer.borrow().custody,
            RecoveryCustody::ReleasedWithPendingTasks
        );
        assert_eq!(observer.borrow().phase, SupervisorPhase::Returned);
        assert_eq!(Arc::strong_count(&resources), 1);
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
    #[tokio::test]
    async fn external_store_retains_actual_aborted_context_until_explicit_transfer() {
        let store = StreamRecoveryStore::new(1, std::time::Duration::from_secs(60));
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("external-store-abort")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let (id, owner) = store
            .admit(
                graph.clone(),
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            )
            .unwrap();
        let observer = owner.observe();
        let entered = Arc::new(Notify::new());
        let task_entered = entered.clone();
        let task = tokio::spawn(async move {
            owner
                .initialize_and_drive(
                    67,
                    &Init,
                    None,
                    std::time::Duration::from_secs(5),
                    move |source| async move {
                        let _source = source;
                        task_entered.notify_one();
                        std::future::pending::<StreamDriveResult<()>>().await
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert_eq!(observer.borrow().phase, SupervisorPhase::Interrupted);
        assert_eq!(observer.borrow().custody, RecoveryCustody::Held);
        assert_eq!(store.admission.in_use(), Ok(1));
        let rejected = match store.admit(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("retained recovery must consume capacity"),
        };
        assert_eq!(rejected.reason, RecoveryAdmissionFailure::Capacity);
        store.close().unwrap();
        let retained = store.take_for_reconciliation(id).unwrap().unwrap();
        assert_eq!(store.admission.in_use(), Ok(1));
        {
            let context = retained.inner.context.lock().await;
            assert!(Arc::ptr_eq(&resources, &context.0));
            let task = &context.1.read::<SagaStack>().unwrap().tasks[0];
            assert_eq!(task.input_snapshot, b"67");
            assert!(
                retained
                    .inner
                    .graph
                    .saga_compensation_registry
                    .read()
                    .unwrap()
                    .get(&task.node_id)
                    .is_some()
            );
        }
        assert!(store.take_for_reconciliation(id).unwrap().is_none());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        drop(retained);
        assert_eq!(store.admission.in_use(), Ok(0));
        assert_eq!(
            observer.borrow().custody,
            RecoveryCustody::ReleasedWithPendingTasks
        );
    }
    #[test]
    fn busy_registry_admission_is_prompt_and_does_not_consume_store_capacity() {
        let store = Arc::new(StreamRecoveryStore::new(
            1,
            std::time::Duration::from_secs(60),
        ));
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("busy-registry")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let external = graph.saga_compensation_registry.clone();
        let guard = external.write().unwrap();
        let thread_store = store.clone();
        let thread_resources = resources.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = thread_store.admit(
                graph,
                OwnedStreamContext::try_new(thread_resources, Bus::new()).unwrap(),
            );
            sender.send(result).unwrap();
        });
        let received = receiver.recv_timeout(std::time::Duration::from_secs(2));
        drop(guard); // Also release a regressed blocking read before joining the worker.
        worker.join().unwrap();
        let rejected = match received.unwrap() {
            Err(rejected) => rejected,
            Ok(_) => panic!("busy registry must reject before effects"),
        };
        assert_eq!(rejected.reason, RecoveryAdmissionFailure::RegistryBusy);
        assert_eq!(store.admission.in_use(), Ok(0));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        let (graph, context) = rejected.into_parts();
        let (id, owner) = store.admit(graph, context).unwrap();
        store.close().unwrap();
        drop(owner);
        assert_eq!(store.admission.in_use(), Ok(1));
        drop(store.take_for_reconciliation(id).unwrap().unwrap());
        assert_eq!(store.admission.in_use(), Ok(0));
    }
    #[test]
    fn closed_store_rejects_before_effects_and_preserves_original_request() {
        let store = StreamRecoveryStore::new(1, std::time::Duration::from_secs(60));
        store.close().unwrap();
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("closed-store");
        let registry = graph.saga_compensation_registry.clone();
        let mut bus = Bus::new();
        bus.insert(String::from("private-request-capability"));
        let rejected = match store.admit(
            graph,
            OwnedStreamContext::try_new(resources.clone(), bus).unwrap(),
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("closed store must not admit"),
        };
        assert_eq!(rejected.reason, RecoveryAdmissionFailure::Closed);
        let (graph, context) = rejected.into_parts();
        assert!(Arc::ptr_eq(&registry, &graph.saga_compensation_registry));
        let (returned, bus) = context.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(bus.read::<String>().unwrap(), "private-request-capability");
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        assert_eq!(store.admission.in_use(), Ok(0));
    }
    #[tokio::test]
    async fn expired_abandonment_records_unknown_effect_loss_before_custody_release() {
        let store = StreamRecoveryStore::new(1, std::time::Duration::from_millis(200));
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("expired-owner")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let (id, owner) = store
            .admit(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            )
            .unwrap();
        let observer = owner.observe();
        let report = owner
            .initialize_and_drive(
                71,
                &EndInit(true),
                None,
                std::time::Duration::from_secs(5),
                |source| async move {
                    let observed = crate::ObservedStream::new(
                        source,
                        ranvier_core::streaming::StreamTimeoutConfig {
                            init: None,
                            idle: None,
                            total: None,
                        },
                        None,
                    )
                    .collect_report()
                    .await;
                    StreamDriveResult {
                        value: observed.items,
                        terminal: observed.terminal,
                    }
                },
            )
            .await;
        assert!(matches!(
            report,
            Ok(StreamInitializationReport::Ready { .. })
        ));
        let operator = uuid::Uuid::new_v4();
        let decision = uuid::Uuid::new_v4();
        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
        assert_eq!(
            store.abandon_expired(id, operator, decision),
            Err(RecoveryDispositionFailure::InUse)
        );
        assert_eq!(
            store.abandon_expired(id, uuid::Uuid::nil(), decision),
            Err(RecoveryDispositionFailure::InvalidIdentity)
        );
        drop(owner);
        store.abandon_expired(id, operator, decision).unwrap();
        let records = store.abandonment_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].owner_id, id);
        assert_eq!(records[0].operator_id, operator);
        assert_eq!(records[0].decision_id, decision);
        assert_eq!(records[0].prior.cleanup_complete, None);
        assert_eq!(
            observer.borrow().custody,
            RecoveryCustody::ReleasedWithPendingTasks
        );
        assert_eq!(store.admission.in_use(), Ok(0));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        assert!(!format!("{records:?}").contains("input_snapshot"));
        let graph = Axon::<u32, u32, String, Resources>::start("full-disposition-records");
        let rejected = match store.admit(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("full disposition records must gate admission"),
        };
        assert_eq!(
            rejected.reason,
            RecoveryAdmissionFailure::DispositionCapacity
        );
        assert!(
            !store
                .acknowledge_abandonment(uuid::Uuid::new_v4(), decision)
                .unwrap()
        );
        assert_eq!(store.abandonment_records().unwrap().len(), 1);
        assert!(store.acknowledge_abandonment(id, decision).unwrap());
        let (graph, context) = rejected.into_parts();
        let (next_id, next_owner) = store.admit(graph, context).unwrap();
        drop(next_owner);
        drop(store.take_for_reconciliation(next_id).unwrap());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn retention_overflow_rejects_and_future_deadline_cannot_be_abandoned() {
        for retention in [std::time::Duration::MAX, std::time::Duration::from_secs(60)] {
            let store = StreamRecoveryStore::new(1, retention);
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let graph = Axon::<u32, u32, String, Resources>::start("retention-boundary");
            let result = store.admit(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            );
            if retention == std::time::Duration::MAX {
                let rejected = match result {
                    Err(rejected) => rejected,
                    Ok(_) => panic!("invalid deadline must reject"),
                };
                assert_eq!(rejected.reason, RecoveryAdmissionFailure::InvalidRetention);
                assert_eq!(store.admission.in_use(), Ok(0));
            } else {
                let (id, owner) = result.unwrap();
                drop(owner);
                assert_eq!(
                    store.abandon_expired(id, uuid::Uuid::new_v4(), uuid::Uuid::new_v4()),
                    Err(RecoveryDispositionFailure::NotExpired)
                );
                assert!(store.abandonment_records().unwrap().is_empty());
                assert_eq!(store.admission.in_use(), Ok(1));
            }
            assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        }
    }
    #[tokio::test]
    async fn store_deadline_cancels_pending_driver_and_runs_actual_cleanup() {
        let store = StreamRecoveryStore::new(1, std::time::Duration::from_millis(200));
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("active-store-expiry")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let (_, owner) = store
            .admit(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            )
            .unwrap();
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            owner.initialize_and_drive(
                73,
                &Init,
                None,
                std::time::Duration::from_secs(5),
                |source| async move {
                    let _source = source;
                    std::future::pending::<StreamDriveResult<()>>().await
                },
            ),
        )
        .await
        .unwrap();
        let Ok(StreamInitializationReport::Ready { value, .. }) = report else {
            panic!("expected expiration after Ready");
        };
        assert!(
            matches!(value.consumption, Err(StreamDriverError::Cancelled(context)) if context.reason == ranvier_core::CancellationReason::DeadlineExceeded)
        );
        assert!(value.cleanup.unwrap().is_complete());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        assert!(
            owner
                .inner
                .context
                .lock()
                .await
                .1
                .read::<SagaStack>()
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn already_expired_store_owner_rejects_before_effect_and_driver() {
        let store = StreamRecoveryStore::new(1, std::time::Duration::ZERO);
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("expired-before-entry")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let (_, owner) = store
            .admit(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            )
            .unwrap();
        let drivers = Arc::new(AtomicUsize::new(0));
        let driver_calls = drivers.clone();
        let report = owner
            .initialize_and_drive(
                79,
                &Init,
                None,
                std::time::Duration::from_secs(5),
                move |_| async move {
                    driver_calls.fetch_add(1, Ordering::SeqCst);
                    completed()
                },
            )
            .await;
        assert!(matches!(
            report,
            Ok(StreamInitializationReport::Rejected { .. })
        ));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 0);
        assert_eq!(drivers.load(Ordering::SeqCst), 0);
        assert_eq!(
            owner.observe().borrow().disposition,
            Some(SupervisorDisposition::PrefixRejected)
        );
    }
    #[tokio::test]
    async fn retention_cancels_pending_initializer_without_cancelling_parent_token() {
        let store = StreamRecoveryStore::new(1, std::time::Duration::from_millis(200));
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("initializer-retention")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let (_, owner) = store
            .admit(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            )
            .unwrap();
        let parent = CancellationToken::new();
        let task_token = parent.clone();
        let entered = Arc::new(Notify::new());
        let task_entered = entered.clone();
        let task_owner = owner.clone();
        let drivers = Arc::new(AtomicUsize::new(0));
        let driver_calls = drivers.clone();
        let task = tokio::spawn(async move {
            task_owner
                .initialize_and_drive(
                    83,
                    &PendingInit(task_entered),
                    Some(task_token),
                    std::time::Duration::from_secs(5),
                    move |_| async move {
                        driver_calls.fetch_add(1, Ordering::SeqCst);
                        completed()
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        let Ok(StreamInitializationReport::Cancelled {
            context,
            initialization_cleanup,
            ..
        }) = result
        else {
            panic!("expected initializer deadline cancellation");
        };
        assert_eq!(
            context.reason,
            ranvier_core::CancellationReason::DeadlineExceeded
        );
        assert!(initialization_cleanup.unwrap().is_complete());
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        assert_eq!(drivers.load(Ordering::SeqCst), 0);
        assert!(!parent.is_cancelled());
        assert_eq!(owner.observe().borrow().cleanup_complete, Some(true));
    }
    fn completed() -> StreamDriveResult<()> {
        StreamDriveResult {
            value: (),
            terminal: crate::StreamConsumptionTerminal::Completed,
        }
    }
    #[tokio::test]
    async fn late_cancellation_invokes_frozen_handler_with_live_resources() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("late-cancel")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let external = graph.saga_compensation_registry.clone();
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let token = CancellationToken::new();
        let driver_token = token.clone();
        let report = owner
            .initialize_and_drive(
                31,
                &Init,
                Some(token),
                std::time::Duration::from_secs(5),
                move |source| async move {
                    external.write().unwrap().handlers.clear();
                    driver_token.cancel(ranvier_core::CancellationReason::Explicit);
                    let observed = crate::ObservedStream::new(
                        source,
                        ranvier_core::streaming::StreamTimeoutConfig {
                            init: None,
                            idle: None,
                            total: None,
                        },
                        Some(driver_token),
                    )
                    .collect_report()
                    .await;
                    StreamDriveResult {
                        value: observed.items,
                        terminal: observed.terminal,
                    }
                },
            )
            .await;
        let Ok(StreamInitializationReport::Ready { prefix, value }) = report else {
            panic!("expected initialized consumption");
        };
        let consumption = value.consumption.unwrap();
        assert!(matches!(prefix, crate::ExecutionReport::Unpersisted { .. }));
        assert!(matches!(
            consumption.terminal,
            crate::StreamConsumptionTerminal::Cancelled(_)
        ));
        assert!(consumption.value.is_empty());
        let cleanup = value.cleanup.unwrap();
        assert!(cleanup.is_complete());
        assert_eq!(cleanup.steps.len(), 1);
        assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
        assert!(
            owner
                .inner
                .context
                .lock()
                .await
                .1
                .read::<SagaStack>()
                .unwrap()
                .is_empty()
        );
    }
    #[derive(Clone)]
    struct FaultUndo;
    #[async_trait::async_trait]
    impl Transition<u32, ()> for FaultUndo {
        type Error = String;
        type Resources = Resources;
        async fn run(&self, _: u32, resources: &Resources, bus: &mut Bus) -> Outcome<(), String> {
            assert!(!bus.cancellation_token().unwrap().is_cancelled());
            resources.effects.fetch_add(1, Ordering::SeqCst);
            Outcome::Fault("fixture undo fault".into())
        }
    }
    struct EndInit(bool);
    #[async_trait::async_trait]
    impl StreamingTransition<u32> for EndInit {
        type Item = u32;
        type Error = String;
        type Resources = Resources;
        async fn run_stream(
            &self,
            _: u32,
            _: &Resources,
            _: &mut Bus,
        ) -> Result<RawStream<u32>, String> {
            if self.0 {
                Ok(Box::pin(futures_util::stream::empty()))
            } else {
                Ok(Box::pin(futures_util::stream::pending()))
            }
        }
    }
    #[tokio::test]
    async fn actual_source_end_timeout_and_fault_keep_independent_cleanup_and_inputs() {
        for mode in 0..3 {
            let resources = Arc::new(Resources {
                effects: AtomicUsize::new(0),
            });
            let prefix = Axon::<u32, u32, String, Resources>::start("terminal-matrix")
                .with_saga_policy(SagaPolicy::Enabled);
            let graph = if mode == 2 {
                prefix.then_compensated(Effect, FaultUndo)
            } else {
                prefix.then_compensated(Effect, Undo)
            };
            let owner = OwnedStreamRecovery::try_new(
                graph,
                OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
                &StreamRecoveryAdmission::new(1),
                None,
            )
            .unwrap();
            let report = owner
                .initialize_and_drive(
                    41,
                    &EndInit(mode == 0),
                    None,
                    std::time::Duration::from_secs(5),
                    move |source| async move {
                        let observed = crate::ObservedStream::new(
                            source,
                            ranvier_core::streaming::StreamTimeoutConfig {
                                init: None,
                                idle: None,
                                total: (mode != 0).then_some(std::time::Duration::ZERO),
                            },
                            None,
                        )
                        .collect_report()
                        .await;
                        StreamDriveResult {
                            value: observed.items,
                            terminal: observed.terminal,
                        }
                    },
                )
                .await;
            let Ok(StreamInitializationReport::Ready { value, .. }) = report else {
                panic!("expected initialized consumption");
            };
            let consumption = value.consumption.unwrap();
            let context = owner.inner.context.lock().await;
            assert!(consumption.value.is_empty());
            if mode == 0 {
                assert_eq!(
                    consumption.terminal,
                    crate::StreamConsumptionTerminal::Completed
                );
                assert!(value.cleanup.is_none());
                assert_eq!(context.1.read::<SagaStack>().unwrap().tasks.len(), 1);
                assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
            } else {
                assert_eq!(
                    consumption.terminal,
                    crate::StreamConsumptionTerminal::TimedOut(crate::StreamTimeoutKind::Total)
                );
                let cleanup = value.cleanup.unwrap();
                assert_eq!(cleanup.is_complete(), mode == 1);
                assert_eq!(cleanup.steps.len(), 1);
                assert!(context.1.read::<SagaStack>().unwrap().is_empty());
                assert_eq!(resources.effects.load(Ordering::SeqCst), 2);
                let retained = context
                    .1
                    .read::<crate::SagaRollbackJournal>()
                    .unwrap()
                    .unresolved_tasks();
                assert_eq!(retained.len(), usize::from(mode == 2));
                if mode == 2 {
                    assert_eq!(retained[0].input_snapshot, b"41");
                    assert_eq!(retained[0].node_id, cleanup.steps[0].node_id);
                }
            }
        }
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
    #[derive(Clone)]
    struct Init;
    #[async_trait::async_trait]
    impl StreamingTransition<u32> for Init {
        type Item = u32;
        type Error = String;
        type Resources = Resources;
        async fn run_stream(
            &self,
            _: u32,
            _: &Resources,
            _: &mut Bus,
        ) -> Result<RawStream<u32>, String> {
            Ok(Box::pin(futures_util::stream::pending()))
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
    struct PendingInit(Arc<Notify>);
    #[async_trait::async_trait]
    impl StreamingTransition<u32> for PendingInit {
        type Item = u32;
        type Error = String;
        type Resources = Resources;
        async fn run_stream(
            &self,
            _: u32,
            _: &Resources,
            _: &mut Bus,
        ) -> Result<RawStream<u32>, String> {
            self.0.notify_one();
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn initializer_abort_retains_effect_input_before_ready_without_invoking_driver() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("initializer-abort")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let registry = Arc::downgrade(&owner.inner.graph.saga_compensation_registry);
        let entered = Arc::new(Notify::new());
        let task_entered = entered.clone();
        let drivers = Arc::new(AtomicUsize::new(0));
        let task_drivers = drivers.clone();
        let task_owner = owner.clone();
        let task = tokio::spawn(async move {
            task_owner
                .initialize_and_drive(
                    23,
                    &PendingInit(task_entered),
                    None,
                    std::time::Duration::from_secs(5),
                    move |_| async move {
                        task_drivers.fetch_add(1, Ordering::SeqCst);
                        completed()
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        let context = owner.inner.context.lock().await;
        assert!(Arc::ptr_eq(&resources, &context.0));
        let stack = context.1.read::<SagaStack>().unwrap();
        assert_eq!(stack.tasks.len(), 1);
        assert_eq!(stack.tasks[0].input_snapshot, b"23");
        assert!(
            registry
                .upgrade()
                .unwrap()
                .read()
                .unwrap()
                .get(&stack.tasks[0].node_id)
                .is_some()
        );
        assert_eq!(drivers.load(Ordering::SeqCst), 0);
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        drop(context);
        drop(owner);
        assert!(registry.upgrade().is_none());
        assert_eq!(Arc::strong_count(&resources), 1);
    }
    #[tokio::test]
    async fn actual_abort_retains_bus_resources_inputs_and_graph_without_replay() {
        let resources = Arc::new(Resources {
            effects: AtomicUsize::new(0),
        });
        let graph = Axon::<u32, u32, String, Resources>::start("owned-abort")
            .with_saga_policy(SagaPolicy::Enabled)
            .then_compensated(Effect, Undo);
        let external = graph.saga_compensation_registry.clone();
        let owner = OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), Bus::new()).unwrap(),
            &StreamRecoveryAdmission::new(1),
            None,
        )
        .unwrap();
        let registry = Arc::downgrade(&owner.inner.graph.saga_compensation_registry);
        let entered = Arc::new(Notify::new());
        let task_owner = owner.clone();
        let task_entered = entered.clone();
        let task = tokio::spawn(async move {
            task_owner
                .initialize_and_drive(
                    17,
                    &Init,
                    None,
                    std::time::Duration::from_secs(5),
                    move |source| async move {
                        let _source = source;
                        task_entered.notify_one();
                        std::future::pending::<StreamDriveResult<()>>().await
                    },
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        external.write().unwrap().handlers.clear();
        assert!(!Arc::ptr_eq(
            &external,
            &owner.inner.graph.saga_compensation_registry
        ));
        assert!(
            owner.inner.context.try_lock().is_err(),
            "driver must retain live Bus borrow"
        );
        let concurrent = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            owner.initialize_and_drive(
                99,
                &Init,
                None,
                std::time::Duration::from_secs(5),
                |_| async { completed() },
            ),
        )
        .await
        .unwrap();
        assert!(matches!(concurrent, Err(StreamInvocationAlreadyStarted)));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        let context = owner.inner.context.lock().await;
        assert!(Arc::ptr_eq(&resources, &context.0));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        assert!(
            !context
                .1
                .has::<crate::execution_observation::InvocationTrace>()
        );
        let task = &context.1.read::<SagaStack>().unwrap().tasks[0];
        assert_eq!(task.input_snapshot, b"17");
        assert!(
            registry
                .upgrade()
                .unwrap()
                .read()
                .unwrap()
                .get(&task.node_id)
                .is_some()
        );
        drop(context);
        let repeated = owner
            .initialize_and_drive(
                18,
                &Init,
                None,
                std::time::Duration::from_secs(5),
                |_| async { completed() },
            )
            .await;
        assert!(matches!(repeated, Err(StreamInvocationAlreadyStarted)));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        drop(owner);
        assert!(registry.upgrade().is_none());
        assert_eq!(Arc::strong_count(&resources), 1);
    }
}
