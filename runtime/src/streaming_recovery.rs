//! Internal ownership primitive. Production admission must reserve bounded recovery
//! capacity before constructing this owner; this module does not spawn tasks.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SupervisorObservation {
    pub(crate) phase: SupervisorPhase,
    pub(crate) disposition: Option<SupervisorDisposition>,
    pub(crate) cleanup_complete: Option<bool>,
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

/// A single invocation's exact graph, typed resources and request Bus. An external
/// clone must outlive the supervisor, including forced task abort. No replay API.
pub(crate) struct OwnedStreamRecovery<In, Out, E, Res> {
    inner: Arc<RecoveryInner<In, Out, E, Res>>,
}
struct RecoveryInner<In, Out, E, Res> {
    graph: Axon<In, Out, E, Res>,
    context: Mutex<(Arc<Res>, Bus)>,
    started: AtomicBool,
    observation: tokio::sync::watch::Sender<SupervisorObservation>,
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
}
impl<In, Out, E, Res> std::fmt::Debug for StreamRecoveryAdmissionRejected<In, Out, E, Res> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamRecoveryAdmissionRejected { reason: registry_poisoned }")
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
    ) -> Result<Self, StreamRecoveryAdmissionRejected<In, Out, E, Res>> {
        // Copy handler Arcs under the original read lock before any effects.
        // A poisoned registry has unknown authority: retain caller ownership.
        let handlers = graph
            .saga_compensation_registry
            .read()
            .map(|registry| registry.handlers.clone())
            .map_err(|_| ());
        let handlers = match handlers {
            Ok(handlers) => handlers,
            Err(()) => return Err(StreamRecoveryAdmissionRejected { graph, context }),
        };
        graph.saga_compensation_registry = Arc::new(std::sync::RwLock::new(
            ranvier_core::saga::SagaCompensationRegistry { handlers },
        ));
        Ok(Self {
            inner: Arc::new(RecoveryInner {
                graph,
                context: Mutex::new(context.into_parts()),
                started: AtomicBool::new(false),
                observation: tokio::sync::watch::channel(SupervisorObservation {
                    phase: SupervisorPhase::NotStarted,
                    disposition: None,
                    cleanup_complete: None,
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
    pub(crate) async fn initialize_and_drive<Item, SErr, S, T, F, Fut>(
        &self,
        input: In,
        streaming: &S,
        token: Option<CancellationToken>,
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
        let report = self
            .inner
            .graph
            .initialize_stream_in_scope(input, resources.as_ref(), &mut owner, streaming, token)
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
                let consumption = std::panic::AssertUnwindSafe(async { drive(value).await })
                    .catch_unwind()
                    .await
                    .map_err(|_| StreamDriverError::Panicked);
                let cleanup = if !matches!(&consumption, Ok(observed) if observed.terminal == crate::StreamConsumptionTerminal::Completed)
                {
                    observation.phase(SupervisorPhase::CleaningUp);
                    self.inner
                        .graph
                        .cleanup_stream_initialization(
                            resources.as_ref(),
                            owner.bus(),
                            saga_enabled,
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
                .initialize_and_drive(43, &QueueInit(task_entered), Some(token), move |source| {
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
                })
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
            )
            .unwrap();
            let (sender, _receiver) = tokio::sync::mpsc::channel(1);
            let report = owner
                .initialize_and_drive(47, &PanicInit(mode), None, move |source| {
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
                })
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
            std::future::pending().await
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
            )
            .unwrap();
            let registry = Arc::downgrade(&owner.inner.graph.saga_compensation_registry);
            let mut observer = owner.observe();
            let task_owner = owner.clone();
            let task = tokio::spawn(async move {
                task_owner
                    .initialize_and_drive(53, &Init, None, |source| async move {
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
                    })
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
                    .initialize_and_drive(99, &Init, None, |_| async { completed() })
                    .await,
                Err(StreamInvocationAlreadyStarted)
            ));
            assert_eq!(
                resources.effects.load(Ordering::SeqCst),
                if pending_top { 2 } else { 3 }
            );
            drop(owner);
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
        )
        .unwrap();
        let report = owner
            .initialize_and_drive(61, &EndInit(true), None, |source| async move {
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
            })
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
                .initialize_and_drive(62, &Init, None, |_| async { completed() })
                .await,
            Err(StreamInvocationAlreadyStarted)
        ));
        assert_eq!(*observer.borrow(), before);
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        assert!(!format!("{before:?}").contains("input_snapshot"));
        drop(owner);
        assert_eq!(observer.borrow().phase, SupervisorPhase::Returned);
        assert_eq!(Arc::strong_count(&resources), 1);
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
        )
        .unwrap();
        let token = CancellationToken::new();
        let driver_token = token.clone();
        let report = owner
            .initialize_and_drive(31, &Init, Some(token), move |source| async move {
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
            })
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
            )
            .unwrap();
            let report = owner
                .initialize_and_drive(41, &EndInit(mode == 0), None, move |source| async move {
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
                })
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
        let rejected = match OwnedStreamRecovery::try_new(
            graph,
            OwnedStreamContext::try_new(resources.clone(), bus).unwrap(),
        ) {
            Err(rejected) => rejected,
            Ok(_) => panic!("poisoned authority must not enter"),
        };
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
                .initialize_and_drive(23, &PendingInit(task_entered), None, move |_| async move {
                    task_drivers.fetch_add(1, Ordering::SeqCst);
                    completed()
                })
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
        )
        .unwrap();
        let registry = Arc::downgrade(&owner.inner.graph.saga_compensation_registry);
        let entered = Arc::new(Notify::new());
        let task_owner = owner.clone();
        let task_entered = entered.clone();
        let task = tokio::spawn(async move {
            task_owner
                .initialize_and_drive(17, &Init, None, move |source| async move {
                    let _source = source;
                    task_entered.notify_one();
                    std::future::pending::<StreamDriveResult<()>>().await
                })
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
            owner.initialize_and_drive(99, &Init, None, |_| async { completed() }),
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
            .initialize_and_drive(18, &Init, None, |_| async { completed() })
            .await;
        assert!(matches!(repeated, Err(StreamInvocationAlreadyStarted)));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        drop(owner);
        assert!(registry.upgrade().is_none());
        assert_eq!(Arc::strong_count(&resources), 1);
    }
}
