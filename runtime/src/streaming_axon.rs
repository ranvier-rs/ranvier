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
