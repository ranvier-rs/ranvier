//! Report-aware execution of a configured Axon prefix and terminal stream initializer.
//! Prefix acknowledgments do not certify stream items or late completion.
use crate::axon::BoxFuture;
use crate::streaming_initialization::StreamInitializationReport;
use crate::{ObservedStream, StreamConsumptionReport};
use futures_core::Stream;
use futures_util::StreamExt;
use ranvier_core::schematic::Schematic;
use ranvier_core::streaming::StreamTimeoutConfig;
use ranvier_core::{Bus, CancellationToken};
use std::{fmt::Debug, pin::Pin, sync::Arc};

/// Terminal stream with its typed prefix output retained for rejected execution.
pub struct StreamingAxon<In, Out, Item, E, Res = ()> {
    pub schematic: Schematic,
    pub(crate) stream_executor: StreamExecutorType<In, Out, Item, E, Res>,
    pub timeout_config: Option<StreamTimeoutConfig>,
    pub buffer_size: usize,
    pub(crate) owned_definition: Arc<OwnedStreamDefinition<In, Out, Item, E, Res>>,
}
/// Preserve the exact configured graph and initializer for supervised factories.
/// Never serialize this capability or reconstruct it from the schematic.
pub(crate) struct OwnedStreamDefinition<In, Out, Item, E, Res> {
    pub(crate) prefix: crate::Axon<In, Out, E, Res>,
    pub(crate) initializer: Arc<dyn std::any::Any + Send + Sync>,
    pub(crate) item_maps: Vec<Arc<dyn Fn(Item) -> Item + Send + Sync>>,
}
pub(crate) enum OwnedStreamPlanRejected<In, Out, E, Res> {
    InitializerType { context: OwnedStreamContext<Res> },
    Admission(crate::streaming_recovery::StreamRecoveryAdmissionRejected<In, Out, E, Res>),
}
pub(crate) struct OwnedStreamPlan<In, Out, Item, E, Res, S> {
    pub(crate) owner: crate::streaming_recovery::OwnedStreamRecovery<In, Out, E, Res>,
    initializer: S,
    item_maps: Vec<Arc<dyn Fn(Item) -> Item + Send + Sync>>,
}
impl<In, Out, Item, E, Res> OwnedStreamDefinition<In, Out, Item, E, Res>
where
    In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + Debug + 'static,
    Res: Send + Sync + 'static,
    Item: Send + 'static,
{
    pub(crate) fn try_plan<S, SErr>(&self, context: OwnedStreamContext<Res>,
        admission: &crate::streaming_recovery_admission::StreamRecoveryAdmission,
        expires: Option<std::time::Instant>)
        -> Result<OwnedStreamPlan<In, Out, Item, E, Res, S>, OwnedStreamPlanRejected<In, Out, E, Res>>
    where S: ranvier_core::streaming::StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res> + Clone,
        SErr: Send + Sync + Debug + 'static,
    {
        let initializer = match self.initializer.downcast_ref::<S>() {
            Some(initializer) => initializer.clone(),
            None => return Err(OwnedStreamPlanRejected::InitializerType { context }),
        };
        let owner = crate::streaming_recovery::OwnedStreamRecovery::try_new(self.prefix.clone(), context, admission, expires)
            .map_err(OwnedStreamPlanRejected::Admission)?;
        Ok(OwnedStreamPlan { owner, initializer, item_maps: self.item_maps.clone() })
    }
}
impl<In, Out, Item, E, Res, S> OwnedStreamPlan<In, Out, Item, E, Res, S>
where
    In: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    Out: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + 'static,
    E: Send + Sync + serde::Serialize + serde::de::DeserializeOwned + Debug + 'static,
    Res: Send + Sync + 'static,
    Item: Send + 'static,
{
    pub(crate) fn prepare<T, F, Fut, SErr>(&self, input: In, grace: std::time::Duration, drive: F)
        -> crate::managed_stream_task::PreparedStreamTask<Result<StreamInitializationReport<Out, crate::streaming_recovery::StreamReadyResult<T>, E>, crate::streaming_recovery::StreamInvocationAlreadyStarted>>
    where S: ranvier_core::streaming::StreamingTransition<Out, Item = Item, Error = SErr, Resources = Res> + Clone,
        SErr: Send + Sync + Debug + 'static, T: Send + 'static,
        F: FnOnce(Pin<Box<dyn Stream<Item = Item> + Send>>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = crate::streaming_recovery::StreamDriveResult<T>> + Send + 'static,
    {
        let maps = self.item_maps.clone();
        self.owner.prepare_managed(input, self.initializer.clone(), grace, move |stream| {
            let mapped = stream.map(move |item| maps.iter().fold(item, |item, map| map(item)));
            drive(Box::pin(mapped))
        })
    }
}

pub type StreamExecutorType<In, Out, Item, E, Res> = Arc<
    dyn for<'a> Fn(
            In,
            &'a Res,
            &'a mut Bus,
            Option<CancellationToken>,
        ) -> BoxFuture<
            'a,
            StreamInitializationReport<Out, Pin<Box<dyn Stream<Item = Item> + Send>>, E>,
        > + Send
        + Sync,
>;

/// Timeout phase for stream lifecycle observations (not a prefix acknowledgment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTimeoutKind {
    Init,
    Idle,
    Total,
}
impl<In, Out, Item, E, Res> Clone for StreamingAxon<In, Out, Item, E, Res> {
    fn clone(&self) -> Self {
        Self {
            schematic: self.schematic.clone(),
            stream_executor: self.stream_executor.clone(),
            timeout_config: self.timeout_config.clone(),
            buffer_size: self.buffer_size,
            owned_definition: self.owned_definition.clone(),
        }
    }
}
impl<In, Out, Item, E, Res> StreamingAxon<In, Out, Item, E, Res>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    Item: Send + 'static,
    E: Send + Sync + Debug + 'static,
    Res: Send + Sync + 'static,
{
    /// Execute and retain prefix durability, typed rejection and initialization evidence.
    pub async fn execute(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
    ) -> StreamInitializationReport<Out, ObservedStream<Item>, E> {
        let report = (self.stream_executor)(input, resources, bus, None).await;
        report.map_value(|stream| self.wrap_stream(stream, None))
    }
    /// Cancellation inside the prefix waits for its report-aware shielded cleanup.
    /// Cancellation after initialization ends the owned stream independently of caller Bus.
    pub async fn execute_cancellable(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> StreamInitializationReport<Out, ObservedStream<Item>, E> {
        let report = (self.stream_executor)(input, resources, bus, Some(token.clone())).await;
        report.map_value(|stream| self.wrap_stream(stream, Some(token)))
    }
    fn wrap_stream(
        &self,
        stream: Pin<Box<dyn Stream<Item = Item> + Send>>,
        token: Option<CancellationToken>,
    ) -> ObservedStream<Item> {
        ObservedStream::new(
            stream,
            self.timeout_config.clone().unwrap_or(StreamTimeoutConfig {
                init: None,
                idle: None,
                total: None,
            }),
            token,
        )
    }
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }
    pub fn with_timeout(mut self, config: StreamTimeoutConfig) -> Self {
        self.timeout_config = Some(config);
        self
    }
    pub fn export_schematic(&self) -> &Schematic {
        &self.schematic
    }

    /// Transform items only in Ready; rejected prefix results retain their original type.
    pub fn map_items<F>(self, f: F) -> Self
    where
        F: Fn(Item) -> Item + Send + Sync + 'static,
    {
        let previous = self.stream_executor;
        let f = Arc::new(f);
        let mut item_maps = self.owned_definition.item_maps.clone();
        item_maps.push(f.clone());
        let owned_definition = Arc::new(OwnedStreamDefinition {
            prefix: self.owned_definition.prefix.clone(),
            initializer: self.owned_definition.initializer.clone(),
            item_maps,
        });
        let executor: StreamExecutorType<In, Out, Item, E, Res> =
            Arc::new(move |input, resources, bus, token| {
                let previous = previous.clone();
                let f = f.clone();
                Box::pin(async move {
                    previous(input, resources, bus, token)
                        .await
                        .map_value(|stream| {
                            Box::pin(stream.map(move |item| f(item)))
                                as Pin<Box<dyn Stream<Item = Item> + Send>>
                        })
                })
            });
        let mut schematic = self.schematic;
        let node_id = uuid::Uuid::new_v4().to_string();
        let last = schematic.nodes.last().map(|node| node.id.clone());
        schematic.nodes.push(ranvier_core::schematic::Node {
            id: node_id.clone(),
            kind: ranvier_core::schematic::NodeKind::Atom,
            label: "map_items".into(),
            description: Some("Per-item stream transformation".into()),
            input_type: std::any::type_name::<Item>().into(),
            output_type: std::any::type_name::<Item>().into(),
            resource_type: "()".into(),
            metadata: Default::default(),
            bus_capability: None,
            source_location: None,
            position: None,
            compensation_node_id: None,
            input_schema: None,
            output_schema: None,
            item_type: Some(std::any::type_name::<Item>().into()),
            terminal: Some(true),
        });
        if let Some(last) = last {
            schematic.edges.push(ranvier_core::schematic::Edge {
                from: last,
                to: node_id,
                kind: ranvier_core::schematic::EdgeType::Linear,
                label: Some("map".into()),
            });
        }
        Self {
            schematic,
            stream_executor: executor,
            owned_definition,
            timeout_config: self.timeout_config,
            buffer_size: self.buffer_size,
        }
    }
    /// Collect observed Ready items while retaining the prefix report.
    /// Ready describes initialization; its consumption report distinguishes partial
    /// timeout/cancellation from source completion. Neither certifies durability.
    /// The terminal collection owner cannot be chained as an ordinary Axon.
    pub fn collect_into_vec(self) -> CollectedStreamingAxon<In, Out, Item, E, Res> {
        CollectedStreamingAxon { streaming: self }
    }
}

pub struct CollectedStreamingAxon<In, Out, Item, E, Res = ()> {
    streaming: StreamingAxon<In, Out, Item, E, Res>,
}
impl<In, Out, Item, E, Res> CollectedStreamingAxon<In, Out, Item, E, Res>
where
    In: Send + Sync + 'static,
    Out: Send + Sync + 'static,
    Item: Send + 'static,
    E: Send + Sync + Debug + 'static,
    Res: Send + Sync + 'static,
{
    pub fn export_schematic(&self) -> &Schematic {
        self.streaming.export_schematic()
    }
    pub async fn execute(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
    ) -> StreamInitializationReport<Out, StreamConsumptionReport<Item>, E> {
        Self::collect_initialized(self.streaming.execute(input, resources, bus).await).await
    }
    /// Preserve partial items and an observed cancellation terminal through the
    /// same configured prefix and initializer as ordinary streaming execution.
    pub async fn execute_cancellable(
        &self,
        input: In,
        resources: &Res,
        bus: &mut Bus,
        token: CancellationToken,
    ) -> StreamInitializationReport<Out, StreamConsumptionReport<Item>, E> {
        Self::collect_initialized(
            self.streaming
                .execute_cancellable(input, resources, bus, token)
                .await,
        )
        .await
    }
    async fn collect_initialized(
        report: StreamInitializationReport<Out, ObservedStream<Item>, E>,
    ) -> StreamInitializationReport<Out, StreamConsumptionReport<Item>, E> {
        match report {
            StreamInitializationReport::Ready { prefix, value } => {
                StreamInitializationReport::Ready {
                    prefix,
                    value: value.collect_report().await,
                }
            }
            report => report.map_value(|_| unreachable!("Ready is handled above")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Axon, ExecutionReport, ExecutionTerminal};
    use ranvier_core::{Outcome, streaming::StreamingTransition};
    #[derive(Clone)]
    struct Chunks {
        items: Vec<String>,
        pending: bool,
        include_input: bool,
    }
    #[async_trait::async_trait]
    impl StreamingTransition<String> for Chunks {
        type Item = String;
        type Error = String;
        type Resources = ();
        async fn run_stream(
            &self,
            input: String,
            _: &(),
            _: &mut Bus,
        ) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>, String> {
            if self.pending {
                Ok(Box::pin(futures_util::stream::pending()))
            } else {
                let items: Vec<_> = self
                    .items
                    .iter()
                    .map(|item| {
                        if self.include_input {
                            format!("{item}{input}")
                        } else {
                            item.clone()
                        }
                    })
                    .collect();
                Ok(Box::pin(futures_util::stream::iter(items)))
            }
        }
    }
    fn circuit(items: &[&str]) -> StreamingAxon<String, String, String, String> {
        Axon::<String, String, String>::start("test").then_stream(Chunks {
            items: items.iter().map(|item| item.to_string()).collect(),
            pending: false,
            include_input: false,
        })
    }
    fn ready(
        report: StreamInitializationReport<String, ObservedStream<String>, String>,
    ) -> ObservedStream<String> {
        match report {
            StreamInitializationReport::Ready {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                value,
            } => value,
            _ => panic!("expected unpersisted initialized stream"),
        }
    }
    #[tokio::test]
    async fn test_streaming_axon_basic_execute() {
        let circuit = Axon::<String, String, String>::start("test").then_stream(Chunks {
            items: vec!["chunk1: ".into(), "chunk2: ".into(), "chunk3: ".into()],
            pending: false,
            include_input: true,
        });
        let items = ready(circuit.execute("hello".into(), &(), &mut Bus::new()).await)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], "chunk1: hello");
    }
    #[tokio::test]
    async fn test_streaming_axon_collect_into_vec() {
        let report = circuit(&["a", "b", "c"])
            .collect_into_vec()
            .execute("".into(), &(), &mut Bus::new())
            .await;
        match report {
            StreamInitializationReport::Ready {
                prefix:
                    ExecutionReport::Unpersisted {
                        result: (),
                        cleanup: None,
                    },
                value,
            } => {
                assert_eq!(value.items, vec!["a", "b", "c"]);
                assert_eq!(value.terminal, crate::StreamConsumptionTerminal::Completed);
            }
            _ => panic!("collection lost initialization report"),
        }
    }
    #[tokio::test]
    async fn owned_definition_survives_clone_and_maps_in_actual_execution_order() {
        let original = circuit(&["a"]);
        let cloned = original.clone();
        assert!(Arc::ptr_eq(&original.owned_definition, &cloned.owned_definition));
        let initializer = original.owned_definition.initializer.clone();
        let prefix_nodes = original.owned_definition.prefix.schematic.nodes.len();
        let mapped = cloned.map_items(|item| format!("{item}1")).map_items(|item| format!("{item}2"));
        assert!(Arc::ptr_eq(&initializer, &mapped.owned_definition.initializer));
        assert!(mapped.owned_definition.initializer.downcast_ref::<Chunks>().is_some());
        assert_eq!(mapped.owned_definition.prefix.schematic.nodes.len(), prefix_nodes);
        assert_eq!(mapped.owned_definition.item_maps.len(), 2);
        let expected = mapped.owned_definition.item_maps.iter().fold("a".to_string(), |item, map| map(item));
        let items = ready(mapped.execute("".into(), &(), &mut Bus::new()).await).collect::<Vec<_>>().await;
        assert_eq!(items, vec![expected]);
        assert!(original.owned_definition.item_maps.is_empty());
    }
    #[tokio::test]
    async fn owned_plan_drives_original_items_with_maps_once() {
        let circuit = circuit(&["a"]).map_items(|item| format!("{item}1")).map_items(|item| format!("{item}2"));
        let plan = match circuit.owned_definition.try_plan::<Chunks, String>(
            OwnedStreamContext::try_new(Arc::new(()), Bus::new()).unwrap(),
            &crate::streaming_recovery_admission::StreamRecoveryAdmission::new(1), None) {
            Ok(plan) => plan, Err(_) => panic!("owned plan rejected"),
        };
        let mut prepared = plan.prepare("".into(), std::time::Duration::from_secs(5), |stream| async move {
            crate::streaming_recovery::StreamDriveResult { value: stream.collect::<Vec<_>>().await, terminal: crate::StreamConsumptionTerminal::Completed }
        });
        let root = crate::nested_custody::CustodyStore::new(1);
        let journal = crate::SagaRollbackJournal::new("owned-plan-parent");
        plan.owner.register_and_start(root.client(), &journal, CancellationToken::new(), &mut prepared).await
            .unwrap_or_else(|error| panic!("{}", error.signal));
        let holder = prepared.holder();
        let joined = holder.lock().await.join_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await.unwrap().unwrap().unwrap();
        match joined {
            Ok(StreamInitializationReport::Ready { value, .. }) => {
                let consumed = value.consumption.unwrap();
                assert_eq!(consumed.value, vec!["a12"]);
                assert_eq!(consumed.terminal, crate::StreamConsumptionTerminal::Completed);
            }
            _ => panic!("original report not ready"),
        }
    }
    #[test]
    fn owned_plan_type_rejection_returns_original_context() {
        let original = circuit(&["a"]);
        let definition = OwnedStreamDefinition {
            prefix: original.owned_definition.prefix.clone(),
            initializer: Arc::new(()), item_maps: Vec::new(),
        };
        let resources = Arc::new(());
        let mut bus = Bus::new();
        bus.insert(139u32);
        let context = OwnedStreamContext::try_new(resources.clone(), bus).unwrap();
        let rejection = match definition.try_plan::<Chunks, String>(context,
            &crate::streaming_recovery_admission::StreamRecoveryAdmission::new(1), None) {
            Err(OwnedStreamPlanRejected::InitializerType { context }) => context,
            _ => panic!("wrong initializer type must reject before admission"),
        };
        let (returned, bus) = rejection.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(bus.read::<u32>(), Some(&139));
    }
    #[tokio::test]
    async fn prepared_plan_retains_recovery_permit_until_rejected_frame_join() {
        let original = circuit(&["a"]);
        let admission = crate::streaming_recovery_admission::StreamRecoveryAdmission::new(1);
        let plan = match original.owned_definition.try_plan::<Chunks, String>(
            OwnedStreamContext::try_new(Arc::new(()), Bus::new()).unwrap(), &admission, None) {
            Ok(plan) => plan, Err(_) => panic!("owned plan rejected"),
        };
        assert_eq!(admission.in_use().unwrap(), 1);
        let mut prepared = plan.prepare("".into(), std::time::Duration::from_secs(5), |_| async {
            panic!("rejected task cannot drive");
            #[allow(unreachable_code)] crate::streaming_recovery::StreamDriveResult { value: (), terminal: crate::StreamConsumptionTerminal::Completed }
        });
        drop(plan);
        assert_eq!(admission.in_use().unwrap(), 1);
        assert!(matches!(prepared.reject_until(tokio::time::Instant::now() + std::time::Duration::from_secs(5)).await, Ok(Ok(None))));
        assert_eq!(admission.in_use().unwrap(), 0);
        assert!(admission.reserve().is_ok());
    }
    #[test]
    fn owned_plan_capacity_rejection_returns_graph_and_original_context() {
        let original = circuit(&["a"]);
        let resources = Arc::new(());
        let mut bus = Bus::new();
        bus.insert(149u32);
        let context = OwnedStreamContext::try_new(resources.clone(), bus).unwrap();
        let rejection = match original.owned_definition.try_plan::<Chunks, String>(context,
            &crate::streaming_recovery_admission::StreamRecoveryAdmission::new(0), None) {
            Err(OwnedStreamPlanRejected::Admission(rejection)) => rejection,
            _ => panic!("zero capacity must return recovery authority"),
        };
        let (graph, context) = rejection.into_parts();
        assert_eq!(graph.schematic.nodes.len(), original.owned_definition.prefix.schematic.nodes.len());
        let (returned, bus) = context.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(bus.read::<u32>(), Some(&149));
    }
    #[derive(Clone)]
    struct Fault;
    #[async_trait::async_trait]
    impl ranvier_core::Transition<String, String> for Fault {
        type Error = String;
        type Resources = ();
        async fn run(&self, _: String, _: &(), _: &mut Bus) -> Outcome<String, String> {
            Outcome::Fault("step failed".into())
        }
    }
    #[tokio::test]
    async fn test_streaming_axon_pipeline_fault() {
        let circuit = Axon::<String, String, String>::start("fault")
            .then(Fault)
            .then_stream(Chunks {
                items: vec![],
                pending: false,
                include_input: false,
            });
        assert!(
            matches!(circuit.execute("".into(), &(), &mut Bus::new()).await,
            StreamInitializationReport::Rejected { prefix: ExecutionReport::Unpersisted {
                result: ExecutionTerminal::Outcome(Outcome::Fault(ref error)), .. } } if error == "step failed")
        );
    }
    #[tokio::test]
    async fn test_streaming_axon_map_items() {
        let circuit =
            circuit(&["hello test", "world test"]).map_items(|value| value.to_uppercase());
        let items = ready(circuit.execute("".into(), &(), &mut Bus::new()).await)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(items, vec!["HELLO TEST", "WORLD TEST"]);
    }
    #[tokio::test]
    async fn test_streaming_axon_map_items_schematic() {
        let circuit = circuit(&["a"]).map_items(|value| value);
        let node = circuit.export_schematic().nodes.last().unwrap();
        assert_eq!(node.label, "map_items");
        assert!(node.item_type.is_some());
    }
    #[tokio::test]
    async fn test_streaming_axon_clone() {
        let cloned = circuit(&["x"]).clone();
        let items = ready(cloned.execute("".into(), &(), &mut Bus::new()).await)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(items, vec!["x"]);
    }
    #[tokio::test]
    async fn cancellation_can_win_stream_initialization() {
        let token = CancellationToken::new();
        token.cancel(ranvier_core::CancellationReason::Explicit);
        assert!(matches!(
            circuit(&[])
                .execute_cancellable("".into(), &(), &mut Bus::new(), token)
                .await,
            StreamInitializationReport::Rejected {
                prefix: ExecutionReport::Unpersisted {
                    result: ExecutionTerminal::Cancelled(_),
                    ..
                }
            }
        ));
    }
    #[tokio::test]
    async fn cancellation_ends_initialized_stream_between_items() {
        let circuit = Axon::<String, String, String>::start("pending").then_stream(Chunks {
            items: vec![],
            pending: true,
            include_input: false,
        });
        let token = CancellationToken::new();
        let mut value = ready(
            circuit
                .execute_cancellable("".into(), &(), &mut Bus::new(), token.clone())
                .await,
        );
        token.cancel(ranvier_core::CancellationReason::Explicit);
        assert_eq!(value.next().await, None);
    }
}

#[cfg(test)]
mod timeout_wakeup_tests {
    use super::*;
    use futures_util::{
        stream,
        task::{ArcWake, waker},
    };
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
        time::Duration,
    };
    struct WakeCount(AtomicUsize);
    impl ArcWake for WakeCount {
        fn wake_by_ref(this: &Arc<Self>) {
            this.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[tokio::test(start_paused = true)]
    async fn earliest_deadline_and_idle_reset_preserve_total_origin() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let input = stream::poll_fn(move |cx| rx.poll_recv(cx));
        let mut wrapped = Box::pin(ObservedStream::new(
            Box::pin(input),
            StreamTimeoutConfig {
                init: Some(Duration::from_secs(5)),
                idle: Some(Duration::from_secs(4)),
                total: Some(Duration::from_secs(10)),
            },
            None,
        ));
        let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
        let task_waker = waker(counter.clone());
        let mut cx = Context::from_waker(&task_waker);
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Pending);
        tokio::time::advance(Duration::from_secs(3)).await;
        tx.send(1).unwrap();
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(Some(1)));
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Pending);
        tokio::time::advance(Duration::from_secs(3)).await;
        // Past init deadline, but first item switched to idle (deadline 7).
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Pending);
        tx.send(2).unwrap();
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(Some(2)));
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Pending);
        tokio::time::advance(Duration::from_secs(3)).await;
        tx.send(3).unwrap();
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(Some(3)));
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Pending);
        // Idle is now 13, but original total deadline remains 10.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(None));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_deadline_and_normal_eof_are_fused() {
        let config = StreamTimeoutConfig {
            init: Some(Duration::ZERO),
            idle: None,
            total: None,
        };
        let mut expired = Box::pin(ObservedStream::new(
            Box::pin(stream::repeat(1)),
            config,
            None,
        ));
        let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
        let task_waker = waker(counter.clone());
        let mut cx = Context::from_waker(&task_waker);
        assert_eq!(expired.as_mut().poll_next(&mut cx), Poll::Ready(None));
        let mut ended = Box::pin(ObservedStream::new(
            Box::pin(stream::empty::<usize>()),
            StreamTimeoutConfig {
                init: None,
                idle: None,
                total: Some(Duration::from_secs(1)),
            },
            None,
        ));
        assert_eq!(ended.as_mut().poll_next(&mut cx), Poll::Ready(None));
        counter.0.store(0, Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            0,
            "EOF must discard its timer"
        );
        assert_eq!(ended.as_mut().poll_next(&mut cx), Poll::Ready(None));
    }

    #[tokio::test]
    async fn deadline_wins_ready_items_and_dropped_timers_do_not_wake() {
        let delay = Duration::from_millis(20);
        let config = StreamTimeoutConfig {
            init: None,
            idle: None,
            total: Some(delay),
        };
        let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
        let task_waker = waker(counter.clone());
        let mut cx = Context::from_waker(&task_waker);
        let mut ready = Box::pin(ObservedStream::new(
            Box::pin(stream::repeat(7)),
            config.clone(),
            None,
        ));
        assert_eq!(ready.as_mut().poll_next(&mut cx), Poll::Ready(Some(7)));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(ready.as_mut().poll_next(&mut cx), Poll::Ready(None));
        drop(ready);
        counter.0.store(0, Ordering::SeqCst);
        let mut pending = Box::pin(ObservedStream::new(
            Box::pin(stream::pending::<usize>()),
            config,
            None,
        ));
        assert_eq!(pending.as_mut().poll_next(&mut cx), Poll::Pending);
        drop(pending);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(counter.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pending_stream_deadlines_register_their_own_wakeup() {
        for mode in 0..3 {
            let delay = Duration::from_millis(20);
            let config = StreamTimeoutConfig {
                init: (mode == 0).then_some(delay),
                idle: (mode == 1).then_some(delay),
                total: (mode == 2).then_some(delay),
            };
            let input = stream::iter((mode == 1).then_some(7)).chain(stream::pending());
            let mut wrapped = Box::pin(ObservedStream::new(Box::pin(input), config, None));
            let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
            let task_waker = waker(counter.clone());
            let mut cx = Context::from_waker(&task_waker);
            if mode == 1 {
                assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(Some(7)));
            }
            assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Pending);
            // This observation timer does not poll the stream or share its waker.
            tokio::time::sleep(Duration::from_millis(80)).await;
            assert!(
                counter.0.load(Ordering::SeqCst) > 0,
                "deadline mode {mode} never woke its consumer"
            );
            assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(None));
            assert_eq!(wrapped.as_mut().poll_next(&mut cx), Poll::Ready(None));
        }
    }
}

/// Request-owned inputs for supervised streaming admission. Construction executes
/// no effects and does not itself start a supervisor or provide durable recovery.
/// Existing recovery slots require explicit graph/handler reconciliation first.
pub struct OwnedStreamContext<Res> {
    resources: Arc<Res>,
    bus: Bus,
}

/// Rejected admission retains the exact Bus and resource allocation for its caller.
/// No payloads are exposed through Debug.
pub struct OwnedStreamContextRejection<Res> {
    resources: Arc<Res>,
    bus: Bus,
}
impl<Res> std::fmt::Debug for OwnedStreamContextRejection<Res> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedStreamContextRejection { reason: recovery_authority_present }")
    }
}
impl<Res> OwnedStreamContext<Res> {
    /// Accept a Bus with no active invocation or recovery slots. Even empty slots
    /// are rejected because their writable/inherited graph provenance is unknown.
    pub fn try_new(
        resources: Arc<Res>,
        mut bus: Bus,
    ) -> Result<Self, OwnedStreamContextRejection<Res>> {
        if crate::execution_observation::has_stream_recovery_authority(&mut bus) {
            Err(OwnedStreamContextRejection { resources, bus })
        } else {
            Ok(Self { resources, bus })
        }
    }
    /// Return ownership without cloning Bus, resources, or request capabilities.
    pub fn into_parts(self) -> (Arc<Res>, Bus) {
        (self.resources, self.bus)
    }
}
impl<Res> OwnedStreamContextRejection<Res> {
    pub fn into_parts(self) -> (Arc<Res>, Bus) {
        (self.resources, self.bus)
    }
}

#[cfg(test)]
mod owned_context_tests {
    use super::*;
    use ranvier_core::saga::SagaStack;

    #[test]
    fn inherited_empty_recovery_is_rejected_even_when_guard_hides_it() {
        use ranvier_core::{BusAccessPolicy, BusTypeRef};
        let mut parent = Bus::new();
        parent.insert_shared(SagaStack::new());
        let mut child = parent.fork_for_parallel();
        child.insert(String::from("private-domain-capability"));
        child.set_access_policy(
            "request-guard",
            Some(BusAccessPolicy::allow_only(vec![BusTypeRef::of::<u32>()])),
        );
        let rejection = match OwnedStreamContext::try_new(Arc::new(MoveOnlyResource(3)), child) {
            Err(rejected) => rejected,
            Ok(_) => panic!("inherited recovery must be rejected"),
        };
        let (_, mut returned) = rejection.into_parts();
        assert!(
            returned.get::<String>().is_err(),
            "caller guard must remain installed"
        );
        returned.clear_access_policy();
        assert_eq!(
            returned.read::<String>().unwrap(),
            "private-domain-capability"
        );
        assert!(returned.read::<SagaStack>().unwrap().is_empty());
        assert!(
            returned.get_mut::<SagaStack>().is_err(),
            "inherited storage stays readonly"
        );
        assert!(parent.read::<SagaStack>().unwrap().is_empty());
    }
    struct MoveOnlyResource(u32);
    #[test]
    fn admission_preserves_move_only_resources_and_local_request_capabilities() {
        let resources = Arc::new(MoveOnlyResource(17));
        let mut bus = Bus::new();
        bus.insert(String::from("private-request-capability"));
        let context = OwnedStreamContext::try_new(resources.clone(), bus).unwrap();
        let (returned, bus) = context.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(returned.0, 17);
        assert_eq!(bus.read::<String>().unwrap(), "private-request-capability");
    }
    #[test]
    fn admission_rejects_existing_nested_custody_even_empty_and_hidden_by_policy() {
        for store_present in [false, true] {
            let resources = Arc::new(MoveOnlyResource(19));
            let mut bus = Bus::new();
            if store_present { bus.insert(crate::nested_custody::CustodyStore::new(1)); }
            else { bus.insert(crate::nested_custody::RequiredOwnedCustody); }
            bus.set_access_policy("deny-admission", Some(ranvier_core::BusAccessPolicy::allow_only(Vec::new())));
            let rejected = match OwnedStreamContext::try_new(resources.clone(), bus) {
                Err(rejected) => rejected,
                Ok(_) => panic!("nested authority was detached"),
            };
            let (returned, mut bus) = rejected.into_parts();
            assert!(Arc::ptr_eq(&returned, &resources));
            assert!(bus.get::<crate::nested_custody::RequiredOwnedCustody>().is_err());
            let access = bus.take_access_scope();
            assert_eq!(bus.has::<crate::nested_custody::CustodyStore>(), store_present);
            assert_eq!(bus.has::<crate::nested_custody::RequiredOwnedCustody>(), !store_present);
            bus.restore_access_scope(access);
        }
    }
    #[test]
    fn expired_weak_parent_client_cannot_be_detached_as_a_fresh_context() {
        let root = crate::nested_custody::CustodyStore::new(1);
        let client = root.client();
        drop(root);
        let resources = Arc::new(MoveOnlyResource(23));
        let mut bus = Bus::new();
        bus.insert(client);
        bus.set_access_policy("deny-expired-client", Some(ranvier_core::BusAccessPolicy::allow_only(Vec::new())));
        let rejected = match OwnedStreamContext::try_new(resources.clone(), bus) {
            Err(rejected) => rejected,
            Ok(_) => panic!("expired client authority was detached"),
        };
        let (returned, mut bus) = rejected.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert!(bus.get::<crate::nested_custody::WeakCustodyClient>().is_err());
        let access = bus.take_access_scope();
        assert!(bus.has::<crate::nested_custody::WeakCustodyClient>());
        bus.restore_access_scope(access);
    }
    #[test]
    fn rejected_recovery_keeps_exact_inputs_and_redacts_debug() {
        let resources = Arc::new(MoveOnlyResource(17));
        let mut bus = Bus::new();
        let mut stack = SagaStack::new();
        stack.push(
            "node".into(),
            "secret-label".into(),
            b"secret-input".to_vec(),
        );
        bus.insert(stack);
        let rejected = match OwnedStreamContext::try_new(resources.clone(), bus) {
            Err(rejected) => rejected,
            Ok(_) => panic!("recovery authority must not be detached"),
        };
        assert!(!format!("{rejected:?}").contains("secret"));
        let (returned, bus) = rejected.into_parts();
        assert!(Arc::ptr_eq(&resources, &returned));
        assert_eq!(
            bus.read::<SagaStack>().unwrap().tasks[0].input_snapshot,
            b"secret-input"
        );
    }
}
