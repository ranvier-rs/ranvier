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

/// A single invocation's exact graph, typed resources and request Bus. An external
/// clone must outlive the supervisor, including forced task abort. No replay API.
pub(crate) struct OwnedStreamRecovery<In, Out, E, Res> {
    inner: Arc<RecoveryInner<In, Out, E, Res>>,
}
struct RecoveryInner<In, Out, E, Res> {
    graph: Axon<In, Out, E, Res>,
    context: Mutex<(Arc<Res>, Bus)>,
    started: AtomicBool,
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
            }),
        })
    }
    /// Keeps InvocationOwner live until the Ready driver returns or is dropped.
    /// This primitive does not classify late terminal/cleanup or authorize retry.
    pub(crate) async fn initialize_and_drive<Item, SErr, S, T, F, Fut>(
        &self,
        input: In,
        streaming: &S,
        token: Option<CancellationToken>,
        drive: F,
    ) -> Result<StreamInitializationReport<Out, T, E>, StreamInvocationAlreadyStarted>
    where
        Item: Send + 'static,
        SErr: Send + Sync + std::fmt::Debug + 'static,
        S: StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res>,
        F: FnOnce(RawStream<Item>) -> Fut,
        Fut: Future<Output = T>,
    {
        if self
            .inner
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(StreamInvocationAlreadyStarted);
        }
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
                StreamInitializationReport::Ready {
                    prefix,
                    value: drive(value).await,
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
        drop(owner);
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
        async fn run(&self, _: u32, resources: &Resources, _: &mut Bus) -> Outcome<(), String> {
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
                    std::future::pending::<()>().await
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert!(
            owner.inner.context.try_lock().is_err(),
            "driver must retain live Bus borrow"
        );
        let concurrent = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            owner.initialize_and_drive(99, &Init, None, |_| async {}),
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
            .initialize_and_drive(18, &Init, None, |_| async {})
            .await;
        assert!(matches!(repeated, Err(StreamInvocationAlreadyStarted)));
        assert_eq!(resources.effects.load(Ordering::SeqCst), 1);
        drop(owner);
        assert!(registry.upgrade().is_none());
        assert_eq!(Arc::strong_count(&resources), 1);
    }
}
